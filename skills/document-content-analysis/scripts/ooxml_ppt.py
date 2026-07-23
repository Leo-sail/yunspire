#!/usr/bin/env python3
"""Loss-preserving PPTX extraction using only Python's standard library."""

from __future__ import annotations

import base64
import hashlib
import json
import math
import mimetypes
import posixpath
import re
import zipfile
from pathlib import Path, PurePosixPath
from urllib.parse import unquote, urlsplit
from xml.etree import ElementTree as ET

from external_image_localizer import (
    ExternalImageLocalizer,
    localization_failure,
    localization_summary,
    public_asset,
)

P = "http://schemas.openxmlformats.org/presentationml/2006/main"
A = "http://schemas.openxmlformats.org/drawingml/2006/main"
R = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
NS = {"p": P, "a": A, "r": R}

IDENTITY = (1.0, 0.0, 0.0, 1.0, 0.0, 0.0)
URL_RE = re.compile(r"""(?i)\bhttps?://[^\s<>"'，。！？；、（）()\[\]{}]+""")
TRAILING_URL_PUNCTUATION = ".,;:!?，。！？；：、"


def qn(namespace, local):
    return f"{{{namespace}}}{local}"


def local_name(tag):
    return tag.rsplit("}", 1)[-1]


def as_int(value, default=0):
    try:
        return int(value)
    except (TypeError, ValueError):
        return default


def truthy(value):
    return str(value or "").strip().lower() in {"1", "true", "on", "yes"}


def multiply(outer, inner):
    """Return affine transform outer(inner(point))."""
    a1, b1, c1, d1, e1, f1 = outer
    a2, b2, c2, d2, e2, f2 = inner
    return (
        a1 * a2 + c1 * b2,
        b1 * a2 + d1 * b2,
        a1 * c2 + c1 * d2,
        b1 * c2 + d1 * d2,
        a1 * e2 + c1 * f2 + e1,
        b1 * e2 + d1 * f2 + f1,
    )


def translate(x, y):
    return (1.0, 0.0, 0.0, 1.0, float(x), float(y))


def scale(x, y):
    return (float(x), 0.0, 0.0, float(y), 0.0, 0.0)


def rotate(degrees):
    radians = math.radians(float(degrees))
    cosine = math.cos(radians)
    sine = math.sin(radians)
    return (cosine, sine, -sine, cosine, 0.0, 0.0)


def apply_matrix(matrix, x, y):
    a, b, c, d, e, f = matrix
    return a * x + c * y + e, b * x + d * y + f


def around_center(cx, cy, degrees, flip_h, flip_v):
    orientation = multiply(
        rotate(degrees),
        scale(-1.0 if flip_h else 1.0, -1.0 if flip_v else 1.0),
    )
    return multiply(
        translate(cx, cy),
        multiply(orientation, translate(-cx, -cy)),
    )


def bbox_from_points(points):
    xs = [point[0] for point in points]
    ys = [point[1] for point in points]
    left, right = min(xs), max(xs)
    top, bottom = min(ys), max(ys)
    return {
        "x": int(round(left)),
        "y": int(round(top)),
        "cx": max(0, int(round(right - left))),
        "cy": max(0, int(round(bottom - top))),
    }


def empty_bbox():
    return {"x": None, "y": None, "cx": None, "cy": None}


def normalized_bbox(bbox, slide_size):
    if not bbox or bbox.get("x") is None:
        return {"x": None, "y": None, "w": None, "h": None}
    width = max(1, slide_size["cx"])
    height = max(1, slide_size["cy"])
    return {
        "x": round(bbox["x"] / width, 8),
        "y": round(bbox["y"] / height, 8),
        "w": round(bbox["cx"] / width, 8),
        "h": round(bbox["cy"] / height, 8),
    }


def valid_http_url(value):
    try:
        parsed = urlsplit(value)
    except ValueError:
        return False
    return parsed.scheme.lower() in {"http", "https"} and bool(parsed.netloc)


def plain_urls(value):
    output = []
    seen = set()
    for match in URL_RE.finditer(value or ""):
        url = match.group(0).rstrip(TRAILING_URL_PUNCTUATION)
        if url and url not in seen and valid_http_url(url):
            seen.add(url)
            output.append(url)
    return output


def stream_sha256(path):
    digest = hashlib.sha256()
    with Path(path).open("rb") as source:
        while True:
            chunk = source.read(1024 * 1024)
            if not chunk:
                break
            digest.update(chunk)
    return digest.hexdigest()


class PptxExtractor:
    def __init__(self, path, external_asset_directory=None):
        self.path = Path(path).expanduser().resolve()
        self.archive = None
        self.names = set()
        self.warnings = []
        self.warning_set = set()
        self.integrity_errors = []
        self.integrity_error_set = set()
        self.relationship_cache = {}
        self.placeholder_cache = {}
        self.content_type_defaults = {}
        self.content_type_overrides = {}
        self.assets = {}
        self.asset_bytes = {}
        self.external_images = ExternalImageLocalizer(
            external_asset_directory
        )
        self.links = []
        self.link_index = {}
        self.current_slide = None
        self.current_source_layer = "slide"
        self.current_source_part = None
        self.element_counter = 0
        self.z_counter = 0
        self.declared_slide_count = 0

    def close(self):
        self.external_images.close()

    def warn(self, message):
        message = str(message).strip()
        if message and message not in self.warning_set:
            self.warning_set.add(message)
            self.warnings.append(message)

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
        if identity not in self.integrity_error_set:
            self.integrity_error_set.add(identity)
            self.integrity_errors.append(record)

    def integrity_output(self, checks):
        return {
            "status": (
                "incomplete" if self.integrity_errors else "complete"
            ),
            "errors": list(self.integrity_errors),
            "checks": dict(checks),
        }

    def read_xml(self, part, required=False):
        if part not in self.names:
            if required:
                self.warn(f"PPTX 缺少必需部件：{part}")
                self.fail(
                    "ppt_required_part_missing",
                    f"PPTX 缺少必需部件：{part}",
                    source_part=part,
                )
            return None
        try:
            return ET.fromstring(self.archive.read(part))
        except (KeyError, OSError, ET.ParseError) as exc:
            self.warn(f"无法解析 PPTX XML {part}：{exc}")
            self.fail(
                "ppt_xml_part_unreadable",
                f"无法解析 PPTX XML {part}：{exc}",
                source_part=part,
            )
            return None

    def validate_paths(self):
        for info in self.archive.infolist():
            normalized = posixpath.normpath(info.filename.replace("\\", "/"))
            if (
                normalized == ".."
                or normalized.startswith("../")
                or normalized.startswith("/")
            ):
                raise ValueError(f"PPTX 包含不安全路径：{info.filename}")

    def load_content_types(self):
        root = self.read_xml("[Content_Types].xml", required=True)
        if root is None:
            return
        for node in root:
            if local_name(node.tag) == "Default":
                extension = node.attrib.get("Extension", "").lower()
                if extension:
                    self.content_type_defaults[extension] = node.attrib.get(
                        "ContentType", "application/octet-stream"
                    )
            elif local_name(node.tag) == "Override":
                part = node.attrib.get("PartName", "").lstrip("/")
                if part:
                    self.content_type_overrides[part] = node.attrib.get(
                        "ContentType", "application/octet-stream"
                    )

    @staticmethod
    def relationship_part(part):
        directory, basename = posixpath.split(part)
        prefix = f"{directory}/" if directory else ""
        return f"{prefix}_rels/{basename}.rels"

    @staticmethod
    def resolve_internal_target(source_part, target):
        target, separator, fragment = str(target or "").partition("#")
        decoded = unquote(target)
        if decoded.startswith("/"):
            resolved = posixpath.normpath(decoded.lstrip("/"))
        else:
            resolved = posixpath.normpath(
                posixpath.join(posixpath.dirname(source_part), decoded)
            )
        if (
            resolved == ".."
            or resolved.startswith("../")
            or resolved.startswith("/")
        ):
            return None, fragment if separator else ""
        return resolved, fragment if separator else ""

    def relationships(self, part):
        if part in self.relationship_cache:
            return self.relationship_cache[part]
        output = {}
        rel_part = self.relationship_part(part)
        root = self.read_xml(rel_part)
        if root is not None:
            for node in root.iter():
                if local_name(node.tag) != "Relationship":
                    continue
                relation_id = node.attrib.get("Id", "")
                if not relation_id:
                    self.fail(
                        "ppt_relationship_id_missing",
                        f"关系部件 {rel_part} 包含缺少 Id 的声明",
                        source_part=part,
                        relationship_part=rel_part,
                    )
                    continue
                if relation_id in output:
                    self.fail(
                        "ppt_relationship_id_duplicate",
                        f"关系部件 {rel_part} 重复声明 {relation_id}",
                        source_part=part,
                        relationship_part=rel_part,
                        relationship_id=relation_id,
                    )
                target = node.attrib.get("Target", "")
                if not target:
                    self.fail(
                        "ppt_relationship_target_missing",
                        f"关系 {relation_id} 缺少 Target",
                        source_part=part,
                        relationship_part=rel_part,
                        relationship_id=relation_id,
                    )
                target_mode = node.attrib.get("TargetMode", "Internal")
                resolved = None
                fragment = ""
                if target_mode.lower() != "external":
                    resolved, fragment = self.resolve_internal_target(part, target)
                    relation_kind = str(
                        node.attrib.get("Type", "")
                    ).rstrip("/").rsplit("/", 1)[-1]
                    if relation_kind in {
                        "slide",
                        "slideLayout",
                        "slideMaster",
                        "notesSlide",
                        "image",
                    } and (not resolved or resolved not in self.names):
                        self.fail(
                            "ppt_required_relationship_target_missing",
                            f"必需关系 {relation_id} 指向不存在的部件",
                            source_part=part,
                            relationship_part=rel_part,
                            relationship_id=relation_id,
                            relationship_kind=relation_kind,
                            target=target,
                            target_part=resolved,
                        )
                output[relation_id] = {
                    "id": relation_id,
                    "type": node.attrib.get("Type", ""),
                    "target": target,
                    "target_mode": target_mode,
                    "resolved_target": resolved,
                    "fragment": fragment,
                    "relationship_part": rel_part,
                }
        self.relationship_cache[part] = output
        return output

    def mime_type(self, part):
        if part in self.content_type_overrides:
            return self.content_type_overrides[part]
        extension = PurePosixPath(part).suffix.lower().lstrip(".")
        if extension in self.content_type_defaults:
            return self.content_type_defaults[extension]
        explicit = {
            "emf": "image/emf",
            "wmf": "image/wmf",
            "svg": "image/svg+xml",
            "tif": "image/tiff",
            "tiff": "image/tiff",
        }
        return (
            explicit.get(extension)
            or mimetypes.guess_type(PurePosixPath(part).name)[0]
            or "application/octet-stream"
        )

    def register_asset(self, package_part, reference):
        if not package_part or package_part not in self.names:
            self.warn(f"图片关系指向不存在的包内资源：{package_part or 'unknown'}")
            self.fail(
                "ppt_embedded_image_part_missing",
                f"图片关系指向不存在的包内资源：{package_part or 'unknown'}",
                target_part=package_part,
                source_part=reference.get("source_part"),
                relationship_id=reference.get("relationship_id"),
                element_id=reference.get("element_id"),
            )
            return None
        try:
            data = self.archive.read(package_part)
        except (KeyError, OSError) as exc:
            self.warn(f"无法读取图片资源 {package_part}：{exc}")
            self.fail(
                "ppt_embedded_image_unreadable",
                f"无法读取图片资源 {package_part}：{exc}",
                target_part=package_part,
                source_part=reference.get("source_part"),
                relationship_id=reference.get("relationship_id"),
                element_id=reference.get("element_id"),
            )
            return None

        digest = hashlib.sha256(data).hexdigest()
        asset_id = f"asset-{digest}"
        original_name = PurePosixPath(package_part).name or f"{asset_id}.bin"
        suffix = PurePosixPath(original_name).suffix.lower()
        attachment_name = f"{asset_id[:22]}{suffix}"

        metadata = self.assets.get(asset_id)
        if metadata is None:
            metadata = {
                "asset_id": asset_id,
                "sha256": digest,
                "mime_type": self.mime_type(package_part),
                "size": len(data),
                "attachment_name": attachment_name,
                "package_parts": [],
                "original_names": [],
                "references": [],
            }
            self.assets[asset_id] = metadata
            self.asset_bytes[asset_id] = data

        if package_part not in metadata["package_parts"]:
            metadata["package_parts"].append(package_part)
        if original_name not in metadata["original_names"]:
            metadata["original_names"].append(original_name)
        if reference not in metadata["references"]:
            metadata["references"].append(reference)
        return asset_id

    def register_external_asset(self, target, reference, suggested_name=None):
        localized = self.external_images.localize(
            target, suggested_name=suggested_name
        )
        asset_id = localized["asset_id"]
        metadata = self.assets.get(asset_id)
        if metadata is None:
            metadata = {
                **public_asset(localized),
                "attachment_name": localized.get("name"),
                "package_parts": [],
                "original_names": [
                    localized.get("original_name") or "external-image"
                ],
                "external_sources": [],
                "references": [],
            }
            if localized.get("_local_path"):
                metadata["_local_path"] = localized["_local_path"]
            self.assets[asset_id] = metadata
        if target and target not in metadata.setdefault(
            "external_sources", []
        ):
            metadata["external_sources"].append(target)
        if reference not in metadata["references"]:
            metadata["references"].append(reference)
        failure = localization_failure(localized)
        if failure:
            self.warn(
                "外链图片本地化失败："
                f"{failure.get('message')} "
                f"({failure.get('code')})；来源 {target}"
            )
            self.fail(
                "ppt_required_external_image_unavailable",
                f"外链图片未能本地化：{target}",
                target=target,
                reason_code=failure.get("code"),
                source_part=reference.get("source_part"),
                relationship_id=reference.get("relationship_id"),
                element_id=reference.get("element_id"),
            )
        return asset_id, dict(metadata.get("localization") or {})

    def public_assets(self):
        return [
            {
                key: value
                for key, value in asset.items()
                if key != "_local_path"
            }
            for asset in self.assets.values()
        ]

    def add_link(
        self,
        source_element_id,
        source_kind,
        target,
        display_text="",
        tooltip="",
        relationship_id="",
        target_mode="External",
        relationship_type="",
        source_location="",
        action="",
    ):
        target = str(target or "").strip() or str(action or "").strip()
        if not target:
            return None
        mode = str(target_mode or "Internal")
        kind = (
            "external"
            if mode.lower() == "external" or bool(urlsplit(target).scheme)
            else "internal"
        )
        payload = {
            "source_element_id": source_element_id,
            "source_kind": source_kind,
            "source_location": source_location,
            "target": target,
            "display_text": str(display_text or ""),
            "tooltip": str(tooltip or ""),
            "relationship_id": str(relationship_id or ""),
            "target_mode": mode,
            "relationship_type": str(relationship_type or ""),
            "action": str(action or ""),
        }
        key = json.dumps(
            payload, ensure_ascii=False, sort_keys=True, separators=(",", ":")
        )
        if key in self.link_index:
            return self.link_index[key]

        link_id = (
            "link-"
            + hashlib.sha256(key.encode("utf-8")).hexdigest()[:24]
        )
        link = {
            "link_id": link_id,
            "kind": kind,
            **payload,
            "provenance": {
                "slide_id": (
                    self.current_slide["slide_id"]
                    if self.current_slide
                    else None
                ),
                "slide_part": (
                    self.current_source_part
                    or (self.current_slide["part"] if self.current_slide else None)
                ),
                "source_layer": self.current_source_layer,
            },
        }
        self.links.append(link)
        self.link_index[key] = link_id
        return link_id

    def relationship_link(
        self,
        hlink,
        relationships,
        source_element_id,
        source_kind,
        display_text="",
        source_location="",
    ):
        relation_id = hlink.attrib.get(qn(R, "id"), "")
        relation = relationships.get(relation_id)
        action = hlink.attrib.get("action", "")
        tooltip = hlink.attrib.get("tooltip", "")

        if relation is None:
            if relation_id:
                self.warn(
                    f"{source_element_id} 的超链接关系 {relation_id} 不存在"
                )
            return self.add_link(
                source_element_id,
                source_kind,
                action,
                display_text=display_text,
                tooltip=tooltip,
                relationship_id=relation_id,
                target_mode="Internal",
                source_location=source_location,
                action=action,
            )

        target = (
            relation["target"]
            if relation["target_mode"].lower() == "external"
            else relation["resolved_target"] or relation["target"]
        )
        if relation.get("fragment"):
            target = f"{target}#{relation['fragment']}"

        return self.add_link(
            source_element_id,
            source_kind,
            target,
            display_text=display_text,
            tooltip=tooltip,
            relationship_id=relation_id,
            target_mode=relation["target_mode"],
            relationship_type=relation["type"],
            source_location=source_location,
            action=action,
        )

    def plain_text_links(
        self, text, source_element_id, source_kind, source_location=""
    ):
        output = []
        for url in plain_urls(text):
            link_id = self.add_link(
                source_element_id,
                source_kind,
                url,
                display_text=url,
                target_mode="External",
                source_location=source_location,
            )
            if link_id:
                output.append(link_id)
        return output

    @staticmethod
    def c_nv_pr(node):
        return next(
            (
                child
                for child in node.iter()
                if local_name(child.tag) == "cNvPr"
            ),
            None,
        )

    @staticmethod
    def placeholder(node):
        placeholder = next(
            (
                child
                for child in node.iter()
                if local_name(child.tag) == "ph"
            ),
            None,
        )
        if placeholder is None:
            return None
        return {
            "type": placeholder.attrib.get("type", "body"),
            "idx": placeholder.attrib.get("idx"),
            "orient": placeholder.attrib.get("orient"),
            "size": placeholder.attrib.get("sz"),
        }

    @staticmethod
    def placeholder_keys(placeholder):
        if not placeholder:
            return []
        keys = []
        if placeholder.get("idx") is not None:
            keys.append(("idx", str(placeholder["idx"])))
        keys.append(("type", str(placeholder.get("type") or "body")))
        return keys

    @staticmethod
    def xfrm_for_node(node):
        kind = local_name(node.tag)
        if kind == "grpSp":
            return node.find("./p:grpSpPr/a:xfrm", NS)
        if kind == "graphicFrame":
            return node.find("./p:xfrm", NS)
        if kind in {"sp", "pic", "cxnSp"}:
            return node.find("./p:spPr/a:xfrm", NS)
        return next(
            (
                child
                for child in node.iter()
                if local_name(child.tag) == "xfrm"
            ),
            None,
        )

    @staticmethod
    def xfrm_values(xfrm):
        if xfrm is None:
            return None
        off = next(
            (child for child in xfrm if local_name(child.tag) == "off"),
            None,
        )
        ext = next(
            (child for child in xfrm if local_name(child.tag) == "ext"),
            None,
        )
        if off is None or ext is None:
            return None
        return {
            "x": as_int(off.attrib.get("x")),
            "y": as_int(off.attrib.get("y")),
            "cx": max(0, as_int(ext.attrib.get("cx"))),
            "cy": max(0, as_int(ext.attrib.get("cy"))),
            "rotation_degrees": as_int(xfrm.attrib.get("rot")) / 60000.0,
            "flip_h": truthy(xfrm.attrib.get("flipH")),
            "flip_v": truthy(xfrm.attrib.get("flipV")),
        }

    def bbox_for_node(self, node, parent_matrix=IDENTITY):
        values = self.xfrm_values(self.xfrm_for_node(node))
        if values is None:
            return None, None

        left = values["x"]
        top = values["y"]
        right = left + values["cx"]
        bottom = top + values["cy"]
        center_x = (left + right) / 2.0
        center_y = (top + bottom) / 2.0

        orientation = around_center(
            center_x,
            center_y,
            values["rotation_degrees"],
            values["flip_h"],
            values["flip_v"],
        )
        matrix = multiply(parent_matrix, orientation)
        points = [
            apply_matrix(matrix, left, top),
            apply_matrix(matrix, right, top),
            apply_matrix(matrix, right, bottom),
            apply_matrix(matrix, left, bottom),
        ]
        return bbox_from_points(points), values

    def group_matrix(self, node, parent_matrix=IDENTITY):
        xfrm = self.xfrm_for_node(node)
        values = self.xfrm_values(xfrm)
        if values is None or xfrm is None:
            return parent_matrix, values

        child_off = next(
            (
                child
                for child in xfrm
                if local_name(child.tag) == "chOff"
            ),
            None,
        )
        child_ext = next(
            (
                child
                for child in xfrm
                if local_name(child.tag) == "chExt"
            ),
            None,
        )
        if child_off is None or child_ext is None:
            return parent_matrix, values

        child_x = as_int(child_off.attrib.get("x"))
        child_y = as_int(child_off.attrib.get("y"))
        child_cx = max(1, as_int(child_ext.attrib.get("cx"), 1))
        child_cy = max(1, as_int(child_ext.attrib.get("cy"), 1))

        base = multiply(
            translate(values["x"], values["y"]),
            multiply(
                scale(values["cx"] / child_cx, values["cy"] / child_cy),
                translate(-child_x, -child_y),
            ),
        )
        center_x = values["x"] + values["cx"] / 2.0
        center_y = values["y"] + values["cy"] / 2.0
        orientation = around_center(
            center_x,
            center_y,
            values["rotation_degrees"],
            values["flip_h"],
            values["flip_v"],
        )
        return multiply(
            parent_matrix, multiply(orientation, base)
        ), values

    def placeholder_map(self, part):
        if not part:
            return {}
        if part in self.placeholder_cache:
            return self.placeholder_cache[part]

        output = {}
        root = self.read_xml(part, required=True)
        if root is None:
            self.placeholder_cache[part] = output
            return output

        shape_tree = next(
            (
                node
                for node in root.iter()
                if local_name(node.tag) == "spTree"
            ),
            None,
        )

        def walk(container, matrix):
            for node in list(container):
                kind = local_name(node.tag)
                if kind == "grpSp":
                    child_matrix, _ = self.group_matrix(node, matrix)
                    walk(node, child_matrix)
                    continue
                if kind not in {"sp", "pic", "graphicFrame", "cxnSp"}:
                    continue
                placeholder = self.placeholder(node)
                bbox, transform = self.bbox_for_node(node, matrix)
                if placeholder and bbox:
                    value = {
                        "bbox_emu": bbox,
                        "transform": transform,
                        "source_part": part,
                    }
                    for key in self.placeholder_keys(placeholder):
                        output.setdefault(key, value)

        if shape_tree is not None:
            walk(shape_tree, IDENTITY)
        self.placeholder_cache[part] = output
        return output

    def placeholder_fallbacks(self, slide_relationships):
        layout_relation = next(
            (
                relation
                for relation in slide_relationships.values()
                if relation["type"].endswith("/slideLayout")
                and relation.get("resolved_target")
            ),
            None,
        )
        if layout_relation is None:
            self.fail(
                "ppt_slide_layout_relationship_missing",
                "幻灯片缺少可解析的 slideLayout 关系",
                slide_id=(
                    self.current_slide.get("slide_id")
                    if self.current_slide
                    else None
                ),
                source_part=self.current_source_part,
            )
            return []

        layout_part = layout_relation["resolved_target"]
        output = [("layout", self.placeholder_map(layout_part))]
        layout_relationships = self.relationships(layout_part)
        master_relation = next(
            (
                relation
                for relation in layout_relationships.values()
                if relation["type"].endswith("/slideMaster")
                and relation.get("resolved_target")
            ),
            None,
        )
        if master_relation:
            output.append(
                (
                    "master",
                    self.placeholder_map(master_relation["resolved_target"]),
                )
            )
        else:
            self.fail(
                "ppt_slide_master_relationship_missing",
                f"版式 {layout_part} 缺少可解析的 slideMaster 关系",
                slide_id=(
                    self.current_slide.get("slide_id")
                    if self.current_slide
                    else None
                ),
                source_part=layout_part,
            )
        return output

    def inheritance_parts(self, slide_relationships):
        layout_relation = next(
            (
                relation
                for relation in slide_relationships.values()
                if relation["type"].endswith("/slideLayout")
                and relation.get("resolved_target")
            ),
            None,
        )
        if layout_relation is None:
            self.fail(
                "ppt_slide_layout_relationship_missing",
                "幻灯片缺少可解析的 slideLayout 关系",
                slide_id=(
                    self.current_slide.get("slide_id")
                    if self.current_slide
                    else None
                ),
                source_part=self.current_source_part,
            )
            return []
        layout_part = layout_relation["resolved_target"]
        layout_relationships = self.relationships(layout_part)
        master_relation = next(
            (
                relation
                for relation in layout_relationships.values()
                if relation["type"].endswith("/slideMaster")
                and relation.get("resolved_target")
            ),
            None,
        )
        output = []
        if master_relation:
            output.append(("master", master_relation["resolved_target"]))
        else:
            self.fail(
                "ppt_slide_master_relationship_missing",
                f"版式 {layout_part} 缺少可解析的 slideMaster 关系",
                slide_id=(
                    self.current_slide.get("slide_id")
                    if self.current_slide
                    else None
                ),
                source_part=layout_part,
            )
        output.append(("layout", layout_part))
        return output

    def resolve_bbox(
        self,
        node,
        parent_matrix,
        placeholder,
        fallback_maps,
        slide_size,
        element_id,
    ):
        bbox, transform = self.bbox_for_node(node, parent_matrix)
        source = "slide"

        if bbox is None and placeholder:
            for fallback_name, mapping in fallback_maps:
                match = None
                for key in self.placeholder_keys(placeholder):
                    if key in mapping:
                        match = mapping[key]
                        break
                if match:
                    bbox = dict(match["bbox_emu"])
                    transform = dict(match.get("transform") or {})
                    source = fallback_name
                    break

        if bbox is None:
            bbox = empty_bbox()
            transform = None
            source = "missing"
            self.warn(f"{element_id} 没有可解析的页面坐标")
            self.fail(
                "ppt_element_position_missing",
                f"{element_id} 没有可解析的页面坐标",
                slide_id=(
                    self.current_slide.get("slide_id")
                    if self.current_slide
                    else None
                ),
                element_id=element_id,
                source_part=self.current_source_part,
                source_layer=self.current_source_layer,
            )

        return {
            "bbox_emu": bbox,
            "bbox_normalized": normalized_bbox(bbox, slide_size),
            "bbox_source": source,
            "transform": transform,
        }

    def next_element(self):
        self.element_counter += 1
        self.z_counter += 1
        return (
            f"{self.current_slide['slide_id']}-element-"
            f"{self.element_counter:04d}",
            self.z_counter,
        )

    def shape_metadata(self, node):
        properties = self.c_nv_pr(node)
        if properties is None:
            return {
                "shape_id": None,
                "name": "",
                "title": "",
                "alt_text": "",
            }
        return {
            "shape_id": properties.attrib.get("id"),
            "name": properties.attrib.get("name", ""),
            "title": properties.attrib.get("title", ""),
            "alt_text": (
                properties.attrib.get("descr", "")
                or properties.attrib.get("title", "")
            ),
        }

    def shape_links(self, node, relationships, element_id):
        properties = self.c_nv_pr(node)
        if properties is None:
            return []

        output = []
        for child in properties.iter():
            kind = local_name(child.tag)
            if kind not in {"hlinkClick", "hlinkHover", "hlinkMouseOver"}:
                continue
            link_id = self.relationship_link(
                child,
                relationships,
                element_id,
                "shape",
                display_text=properties.attrib.get("name", ""),
                source_location=kind,
            )
            if link_id and link_id not in output:
                output.append(link_id)

        alt_text = properties.attrib.get("descr", "")
        for link_id in self.plain_text_links(
            alt_text,
            element_id,
            "shape_alt_text",
            "cNvPr@descr",
        ):
            if link_id not in output:
                output.append(link_id)
        return output

    def parse_paragraphs(
        self,
        container,
        relationships,
        element_id,
        source_kind="text",
        prefix="",
    ):
        paragraphs = []
        all_links = []
        paragraph_nodes = [
            node
            for node in container.iter()
            if local_name(node.tag) == "p"
        ]

        for paragraph_index, paragraph in enumerate(paragraph_nodes):
            runs = []
            paragraph_links = []
            p_pr = next(
                (
                    child
                    for child in paragraph
                    if local_name(child.tag) == "pPr"
                ),
                None,
            )
            level = as_int(
                p_pr.attrib.get("lvl") if p_pr is not None else 0
            )

            for run_index, child in enumerate(list(paragraph)):
                kind = local_name(child.tag)
                if kind == "br":
                    runs.append(
                        {"text": "\n", "links": [], "kind": "break"}
                    )
                    continue
                if kind not in {"r", "fld"}:
                    continue

                text_node = next(
                    (
                        node
                        for node in child
                        if local_name(node.tag) == "t"
                    ),
                    None,
                )
                text = (
                    ""
                    if text_node is None or text_node.text is None
                    else text_node.text
                )
                run_links = []
                r_pr = next(
                    (
                        node
                        for node in child
                        if local_name(node.tag)
                        in {"rPr", "endParaRPr"}
                    ),
                    None,
                )

                if r_pr is not None:
                    for hlink in r_pr.iter():
                        hlink_kind = local_name(hlink.tag)
                        if hlink_kind not in {
                            "hlinkClick",
                            "hlinkHover",
                            "hlinkMouseOver",
                        }:
                            continue
                        link_id = self.relationship_link(
                            hlink,
                            relationships,
                            element_id,
                            f"{source_kind}_run",
                            display_text=text,
                            source_location=(
                                f"{prefix}paragraph[{paragraph_index}]"
                                f".run[{run_index}]"
                            ),
                        )
                        if link_id:
                            run_links.append(link_id)

                if not run_links:
                    run_links.extend(
                        self.plain_text_links(
                            text,
                            element_id,
                            f"{source_kind}_plain_url",
                            (
                                f"{prefix}paragraph[{paragraph_index}]"
                                f".run[{run_index}]"
                            ),
                        )
                    )

                runs.append(
                    {
                        "text": text,
                        "links": run_links,
                        "kind": "field" if kind == "fld" else "run",
                    }
                )
                for link_id in run_links:
                    if link_id not in paragraph_links:
                        paragraph_links.append(link_id)

            text = "".join(run["text"] for run in runs)

            # Formatting can split one visible URL across several runs.
            existing_targets = {
                link["target"]
                for link in self.links
                if link["link_id"] in paragraph_links
            }
            for url in plain_urls(text):
                if url in existing_targets:
                    continue
                link_id = self.add_link(
                    element_id,
                    f"{source_kind}_paragraph_plain_url",
                    url,
                    display_text=url,
                    target_mode="External",
                    source_location=f"{prefix}paragraph[{paragraph_index}]",
                )
                if link_id:
                    paragraph_links.append(link_id)

            paragraphs.append(
                {
                    "index": paragraph_index,
                    "level": level,
                    "text": text,
                    "runs": runs,
                    "links": paragraph_links,
                }
            )
            for link_id in paragraph_links:
                if link_id not in all_links:
                    all_links.append(link_id)

        return paragraphs, all_links

    @staticmethod
    def role(placeholder, fallback):
        placeholder_type = str(
            (placeholder or {}).get("type") or ""
        ).strip()
        return placeholder_type or fallback

    def common_element(
        self,
        node,
        element_id,
        z_order,
        parent_matrix,
        group_path,
        fallback_maps,
        slide_size,
        forced_bbox=None,
    ):
        metadata = self.shape_metadata(node)
        placeholder = self.placeholder(node)
        geometry = (
            {
                "bbox_emu": dict(forced_bbox),
                "bbox_normalized": normalized_bbox(forced_bbox, slide_size),
                "bbox_source": "slide_background",
                "transform": None,
            }
            if forced_bbox is not None
            else self.resolve_bbox(
                node,
                parent_matrix,
                placeholder,
                fallback_maps,
                slide_size,
                element_id,
            )
        )
        return {
            "element_id": element_id,
            "shape_id": metadata["shape_id"],
            "name": metadata["name"],
            "title": metadata["title"],
            "alt_text": metadata["alt_text"],
            "placeholder": placeholder,
            "group_id": group_path[-1] if group_path else None,
            "group_path": list(group_path),
            "z_order": z_order,
            "reading_order": None,
            "source_layer": self.current_source_layer,
            "source_part": self.current_source_part,
            **geometry,
        }

    def parse_text_shape(
        self,
        node,
        relationships,
        parent_matrix,
        group_path,
        fallback_maps,
        slide_size,
    ):
        text_body = node.find("./p:txBody", NS)
        element_id, z_order = self.next_element()
        common = self.common_element(
            node,
            element_id,
            z_order,
            parent_matrix,
            group_path,
            fallback_maps,
            slide_size,
        )
        paragraphs, run_links = (
            self.parse_paragraphs(
                text_body, relationships, element_id
            )
            if text_body is not None
            else ([], [])
        )
        links = []
        for link_id in (
            self.shape_links(node, relationships, element_id)
            + run_links
        ):
            if link_id not in links:
                links.append(link_id)

        text = "\n".join(
            paragraph["text"] for paragraph in paragraphs
        ).strip()
        if text_body is None and not links:
            return None

        return {
            **common,
            "type": "text" if text_body is not None else "shape",
            "role": self.role(
                common["placeholder"],
                "body" if text_body is not None else "shape",
            ),
            "text": text,
            "paragraphs": paragraphs,
            "links": links,
        }

    def image_relation(self, node, relationships, element_id):
        blip = next(
            (
                child
                for child in node.iter()
                if local_name(child.tag) == "blip"
            ),
            None,
        )
        if blip is None:
            self.fail(
                "ppt_image_blip_missing",
                f"{element_id} 的图片元素缺少 blip 声明",
                slide_id=(
                    self.current_slide.get("slide_id")
                    if self.current_slide
                    else None
                ),
                element_id=element_id,
                source_part=self.current_source_part,
                source_layer=self.current_source_layer,
            )
            return None, None, None, None, {}

        embedded_id = blip.attrib.get(qn(R, "embed"), "")
        linked_id = blip.attrib.get(qn(R, "link"), "")
        relation_id = embedded_id or linked_id
        relation = relationships.get(relation_id)

        if relation is None:
            self.warn(
                f"{element_id} 的图片关系 "
                f"{relation_id or 'unknown'} 不存在"
            )
            self.fail(
                "ppt_image_relationship_missing",
                f"{element_id} 的图片关系 {relation_id or 'unknown'} 不存在",
                slide_id=(
                    self.current_slide.get("slide_id")
                    if self.current_slide
                    else None
                ),
                element_id=element_id,
                source_part=self.current_source_part,
                source_layer=self.current_source_layer,
                relationship_id=relation_id or None,
            )
            return None, relation_id or None, None, None, {}

        if not str(relation.get("type") or "").endswith("/image"):
            self.fail(
                "ppt_image_relationship_type_invalid",
                f"{element_id} 的图片关系 {relation_id or 'unknown'} 类型不是 image",
                slide_id=(
                    self.current_slide.get("slide_id")
                    if self.current_slide
                    else None
                ),
                element_id=element_id,
                source_part=self.current_source_part,
                relationship_id=relation_id or None,
                relationship_type=relation.get("type"),
            )

        if relation["target_mode"].lower() == "external":
            link_id = self.add_link(
                element_id,
                "external_image",
                relation["target"],
                display_text="外部图片",
                relationship_id=relation_id,
                target_mode=relation["target_mode"],
                relationship_type=relation["type"],
                source_location=f"{self.current_source_layer}.image",
            )
            properties = self.c_nv_pr(node)
            asset_id, localization = self.register_external_asset(
                relation["target"],
                {
                    "slide_id": self.current_slide["slide_id"],
                    "element_id": element_id,
                    "relationship_id": relation_id,
                    "source_layer": self.current_source_layer,
                    "source_part": self.current_source_part,
                },
                suggested_name=(
                    properties.attrib.get("name")
                    if properties is not None
                    else None
                ),
            )
            return (
                asset_id,
                relation_id,
                relation["target"],
                link_id,
                localization,
            )

        asset_id = self.register_asset(
            relation["resolved_target"],
            {
                "slide_id": self.current_slide["slide_id"],
                "element_id": element_id,
                "relationship_id": relation_id,
                "source_layer": self.current_source_layer,
                "source_part": self.current_source_part,
            },
        )
        return asset_id, relation_id, None, None, {}

    def parse_image(
        self,
        node,
        relationships,
        parent_matrix,
        group_path,
        fallback_maps,
        slide_size,
        role="image",
        forced_bbox=None,
        forced_z=None,
    ):
        element_id, generated_z = self.next_element()
        z_order = generated_z if forced_z is None else forced_z
        common = self.common_element(
            node,
            element_id,
            z_order,
            parent_matrix,
            group_path,
            fallback_maps,
            slide_size,
            forced_bbox,
        )

        (
            asset_id,
            relation_id,
            external_source,
            external_link_id,
            external_localization,
        ) = (
            self.image_relation(node, relationships, element_id)
        )
        source_rect = next(
            (
                child
                for child in node.iter()
                if local_name(child.tag) == "srcRect"
            ),
            None,
        )
        crop_ooxml = {
            edge: as_int(source_rect.attrib.get(edge))
            for edge in ("l", "t", "r", "b")
            if source_rect is not None and edge in source_rect.attrib
        }
        crop = {edge: value / 1000.0 for edge, value in crop_ooxml.items()}
        crop_fraction = {edge: value / 100000.0 for edge, value in crop_ooxml.items()}
        if asset_id in self.assets:
            for reference in self.assets[asset_id].get("references", []):
                if reference.get("element_id") == element_id:
                    reference["bbox_emu"] = dict(common["bbox_emu"])
                    reference["bbox_normalized"] = dict(common["bbox_normalized"])
                    reference["crop_percent"] = dict(crop)
        return {
            **common,
            "type": "image",
            "role": role,
            "asset_id": asset_id,
            "image_relationship_id": relation_id,
            "external_source": external_source,
            "external_source_auto_fetch": bool(
                external_localization.get("status") == "localized"
            ),
            "external_image_localization": external_localization,
            "crop_ooxml": crop_ooxml,
            "crop_percent": crop,
            "crop_fraction": crop_fraction,
            "links": list(dict.fromkeys(
                self.shape_links(node, relationships, element_id)
                + ([external_link_id] if external_link_id else [])
            )),
            "spatial_relation_candidate_ids": [],
        }

    def parse_table(
        self,
        node,
        relationships,
        parent_matrix,
        group_path,
        fallback_maps,
        slide_size,
    ):
        table = next(
            (
                child
                for child in node.iter()
                if local_name(child.tag) == "tbl"
            ),
            None,
        )
        if table is None:
            return None

        element_id, z_order = self.next_element()
        common = self.common_element(
            node,
            element_id,
            z_order,
            parent_matrix,
            group_path,
            fallback_maps,
            slide_size,
        )
        grid = next(
            (
                child
                for child in table
                if local_name(child.tag) == "tblGrid"
            ),
            None,
        )
        column_widths = (
            [
                max(0, as_int(child.attrib.get("w")))
                for child in grid
            ]
            if grid is not None
            else []
        )
        table_rows = [
            child
            for child in table
            if local_name(child.tag) == "tr"
        ]
        total_width = sum(column_widths)
        total_height = sum(
            max(0, as_int(row.attrib.get("h")))
            for row in table_rows
        )
        table_bbox = common["bbox_emu"]
        rows = []
        links = self.shape_links(
            node, relationships, element_id
        )
        row_heights = [max(0, as_int(row.attrib.get("h"))) for row in table_rows]
        merge_coverage = {}
        maximum_column = len(column_widths)

        for row_index, row in enumerate(table_rows):
            row_height = max(0, as_int(row.attrib.get("h")))
            cells = []
            column_cursor = 0

            for physical_index, cell in enumerate(
                child
                for child in row
                if local_name(child.tag) == "tc"
            ):
                grid_span = max(1, as_int(cell.attrib.get("gridSpan"), 1))
                row_span = max(1, as_int(cell.attrib.get("rowSpan"), 1))
                h_merge = truthy(cell.attrib.get("hMerge"))
                v_merge = truthy(cell.attrib.get("vMerge"))
                column_index = column_cursor
                cell_id = f"{element_id}-r{row_index + 1}c{column_index + 1}"
                merge_anchor_id = None
                if h_merge and column_index > 0:
                    merge_anchor_id = merge_coverage.get((row_index, column_index - 1))
                if v_merge and merge_anchor_id is None and row_index > 0:
                    merge_anchor_id = merge_coverage.get((row_index - 1, column_index))
                is_merge_continuation = bool(h_merge or v_merge)
                if is_merge_continuation and merge_anchor_id is None:
                    self.warn(f"{cell_id} 标记为合并延续单元格，但未找到合并起点")
                    merge_anchor_id = cell_id
                if not is_merge_continuation:
                    merge_anchor_id = cell_id
                covered_coordinates = []
                for covered_row in range(row_index, min(len(table_rows), row_index + row_span)):
                    for covered_column in range(column_index, column_index + grid_span):
                        merge_coverage[(covered_row, covered_column)] = merge_anchor_id
                        covered_coordinates.append(
                            {"row": covered_row, "column": covered_column}
                        )
                paragraphs, cell_links = self.parse_paragraphs(
                    cell,
                    relationships,
                    element_id,
                    source_kind="table_cell",
                    prefix=(
                        f"row[{row_index}].cell[{physical_index}]."
                    ),
                )
                for link_id in cell_links:
                    if link_id not in links:
                        links.append(link_id)

                width = sum(column_widths[column_index : column_index + grid_span])
                height = sum(row_heights[row_index : row_index + row_span])
                x_cursor = sum(column_widths[:column_index])
                y_cursor = sum(row_heights[:row_index])
                cell_bbox = empty_bbox()
                if (
                    table_bbox.get("x") is not None
                    and total_width > 0
                    and total_height > 0
                ):
                    cell_bbox = {
                        "x": int(
                            round(
                                table_bbox["x"]
                                + table_bbox["cx"]
                                * x_cursor
                                / total_width
                            )
                        ),
                        "y": int(
                            round(
                                table_bbox["y"]
                                + table_bbox["cy"]
                                * y_cursor
                                / total_height
                            )
                        ),
                        "cx": int(
                            round(
                                table_bbox["cx"]
                                * width
                                / total_width
                            )
                        ),
                        "cy": int(
                            round(
                                table_bbox["cy"]
                                * height
                                / total_height
                            )
                        ),
                    }

                cells.append(
                    {
                        "row": row_index,
                        "column": column_index,
                        "physical_index": physical_index,
                        "cell_id": cell_id,
                        "text": "\n".join(
                            paragraph["text"]
                            for paragraph in paragraphs
                        ).strip(),
                        "paragraphs": paragraphs,
                        "links": cell_links,
                        "bbox_emu": cell_bbox,
                        "bbox_normalized": normalized_bbox(
                            cell_bbox, slide_size
                        ),
                        "grid_span": grid_span,
                        "row_span": row_span,
                        "h_merge": h_merge,
                        "v_merge": v_merge,
                        "is_merge_continuation": is_merge_continuation,
                        "merge_anchor_cell_id": merge_anchor_id,
                        "covered_coordinates": covered_coordinates,
                    }
                )
                column_cursor += grid_span
                maximum_column = max(maximum_column, column_cursor)

            rows.append(
                {
                    "index": row_index,
                    "height_emu": row_height,
                    "cells": cells,
                }
            )
        return {
            **common,
            "type": "table",
            "role": self.role(common["placeholder"], "table"),
            "column_widths_emu": column_widths,
            "column_count": maximum_column,
            "rows": rows,
            "links": links,
        }

    def parse_group(
        self,
        node,
        slide,
        relationships,
        parent_matrix,
        group_path,
        fallback_maps,
        slide_size,
    ):
        metadata = self.shape_metadata(node)
        number = (
            metadata.get("shape_id")
            or len(slide["groups"]) + 1
        )
        group_id = f"{slide['slide_id']}-group-{number}"
        bbox, transform = self.bbox_for_node(
            node, parent_matrix
        )
        child_matrix, group_transform = self.group_matrix(
            node, parent_matrix
        )
        slide["groups"].append(
            {
                "group_id": group_id,
                "parent_group_id": (
                    group_path[-1] if group_path else None
                ),
                "name": metadata.get("name", ""),
                "shape_id": metadata.get("shape_id"),
                "bbox_emu": bbox or empty_bbox(),
                "bbox_normalized": normalized_bbox(
                    bbox, slide_size
                ),
                "transform": transform or group_transform,
            }
        )
        self.parse_container(
            node,
            slide,
            relationships,
            child_matrix,
            [*group_path, group_id],
            fallback_maps,
            slide_size,
        )

    def parse_container(
        self,
        container,
        slide,
        relationships,
        parent_matrix,
        group_path,
        fallback_maps,
        slide_size,
    ):
        for node in list(container):
            kind = local_name(node.tag)
            if kind == "grpSp":
                self.parse_group(
                    node,
                    slide,
                    relationships,
                    parent_matrix,
                    group_path,
                    fallback_maps,
                    slide_size,
                )
                continue

            parsed_elements = []
            if kind in {"sp", "cxnSp"}:
                element = self.parse_text_shape(
                    node,
                    relationships,
                    parent_matrix,
                    group_path,
                    fallback_maps,
                    slide_size,
                )
                if element is not None:
                    parsed_elements.append(element)
                if kind == "sp" and any(local_name(child.tag) == "blip" for child in node.iter()):
                    parsed_elements.append(
                        self.parse_image(
                            node,
                            relationships,
                            parent_matrix,
                            group_path,
                            fallback_maps,
                            slide_size,
                            role="shape_fill_image",
                        )
                    )
            elif kind == "pic":
                parsed_elements.append(self.parse_image(
                    node,
                    relationships,
                    parent_matrix,
                    group_path,
                    fallback_maps,
                    slide_size,
                ))
            elif kind == "graphicFrame":
                element = self.parse_table(
                    node,
                    relationships,
                    parent_matrix,
                    group_path,
                    fallback_maps,
                    slide_size,
                )
                if element is not None:
                    parsed_elements.append(element)
                else:
                    self.fail(
                        "ppt_graphic_frame_unparsed",
                        "幻灯片包含无法忠实解析的 graphicFrame",
                        slide_id=slide.get("slide_id"),
                        source_part=self.current_source_part,
                        source_layer=self.current_source_layer,
                    )
            elif kind not in {"nvGrpSpPr", "grpSpPr", "extLst"}:
                self.fail(
                    "ppt_slide_element_unparsed",
                    f"幻灯片包含无法忠实解析的元素 {kind}",
                    slide_id=slide.get("slide_id"),
                    source_part=self.current_source_part,
                    source_layer=self.current_source_layer,
                    element_type=kind,
                )

            slide["elements"].extend(parsed_elements)

    def parse_background(
        self, root, slide, relationships, slide_size
    ):
        background = next(
            (
                node
                for node in root.iter()
                if local_name(node.tag) == "bg"
            ),
            None,
        )
        if background is None:
            return
        blip = next(
            (
                node
                for node in background.iter()
                if local_name(node.tag) == "blip"
            ),
            None,
        )
        if blip is None:
            return

        slide["elements"].append(
            self.parse_image(
                background,
                relationships,
                IDENTITY,
                [],
                [],
                slide_size,
                role="background",
                forced_bbox={
                    "x": 0,
                    "y": 0,
                    "cx": slide_size["cx"],
                    "cy": slide_size["cy"],
                },
                forced_z=-1,
            )
        )

    def parse_inherited_images(self, part, layer, slide, slide_size):
        root = self.read_xml(part, required=True)
        if root is None:
            return
        relationships = self.relationships(part)
        previous_layer = self.current_source_layer
        previous_part = self.current_source_part
        self.current_source_layer = layer
        self.current_source_part = part
        self.parse_background(root, slide, relationships, slide_size)
        shape_tree = next(
            (node for node in root.iter() if local_name(node.tag) == "spTree"),
            None,
        )
        if shape_tree is None:
            self.fail(
                "ppt_inherited_shape_tree_missing",
                f"{layer} 部件 {part} 缺少 spTree",
                slide_id=slide.get("slide_id"),
                source_part=part,
                source_layer=layer,
            )

        def walk(container, parent_matrix, group_path):
            for node in list(container):
                kind = local_name(node.tag)
                if kind == "grpSp":
                    metadata = self.shape_metadata(node)
                    group_id = (
                        f"{slide['slide_id']}-{layer}-group-"
                        f"{metadata.get('shape_id') or len(slide['groups']) + 1}"
                    )
                    bbox, transform = self.bbox_for_node(node, parent_matrix)
                    child_matrix, group_transform = self.group_matrix(node, parent_matrix)
                    slide["groups"].append(
                        {
                            "group_id": group_id,
                            "parent_group_id": group_path[-1] if group_path else None,
                            "name": metadata.get("name", ""),
                            "shape_id": metadata.get("shape_id"),
                            "source_layer": layer,
                            "source_part": part,
                            "bbox_emu": bbox or empty_bbox(),
                            "bbox_normalized": normalized_bbox(bbox, slide_size),
                            "transform": transform or group_transform,
                        }
                    )
                    walk(node, child_matrix, [*group_path, group_id])
                    continue
                has_blip = any(local_name(child.tag) == "blip" for child in node.iter())
                if kind == "pic" or (kind == "sp" and has_blip):
                    slide["elements"].append(
                        self.parse_image(
                            node,
                            relationships,
                            parent_matrix,
                            group_path,
                            [],
                            slide_size,
                            role=f"{layer}_image",
                        )
                    )

        if shape_tree is not None:
            walk(shape_tree, IDENTITY, [])
        self.current_source_layer = previous_layer
        self.current_source_part = previous_part

    def parse_notes(self, relationships):
        declared = [
            value
            for value in relationships.values()
            if value["type"].endswith("/notesSlide")
        ]
        relation = next(
            (
                value
                for value in relationships.values()
                if value["type"].endswith("/notesSlide")
                and value.get("resolved_target")
            ),
            None,
        )
        if relation is None:
            if declared:
                self.fail(
                    "ppt_notes_relationship_unresolved",
                    "幻灯片声明了 notesSlide，但目标部件无法解析",
                    slide_id=self.current_slide.get("slide_id"),
                    source_part=self.current_source_part,
                    relationship_ids=[
                        item.get("id") for item in declared
                    ],
                )
            return "", []

        part = relation["resolved_target"]
        root = self.read_xml(part, required=True)
        if root is None:
            return "", []

        notes_relationships = self.relationships(part)
        notes_element_id = (
            f"{self.current_slide['slide_id']}-speaker-notes"
        )
        paragraphs, links = self.parse_paragraphs(
            root,
            notes_relationships,
            notes_element_id,
            source_kind="speaker_notes",
        )
        text = "\n".join(
            paragraph["text"] for paragraph in paragraphs
        ).strip()
        return text, links

    @staticmethod
    def overlap_length(start_a, length_a, start_b, length_b):
        return max(
            0.0,
            min(start_a + length_a, start_b + length_b)
            - max(start_a, start_b),
        )

    @staticmethod
    def center(bbox):
        return (
            bbox["x"] + bbox["w"] / 2.0,
            bbox["y"] + bbox["h"] / 2.0,
        )

    def build_spatial_candidates(self, slide):
        images = [
            element
            for element in slide["elements"]
            if element["type"] == "image"
            and element["bbox_normalized"]["x"] is not None
        ]
        texts = [
            element
            for element in slide["elements"]
            if element["type"] == "text"
            and element.get("text", "").strip()
            and element["bbox_normalized"]["x"] is not None
        ]
        selected_candidates = []

        for image in images:
            image_box = image["bbox_normalized"]
            image_center = self.center(image_box)
            candidates = []

            for text in texts:
                text_box = text["bbox_normalized"]
                text_center = self.center(text_box)
                x_overlap = self.overlap_length(
                    image_box["x"],
                    image_box["w"],
                    text_box["x"],
                    text_box["w"],
                )
                y_overlap = self.overlap_length(
                    image_box["y"],
                    image_box["h"],
                    text_box["y"],
                    text_box["h"],
                )
                x_ratio = x_overlap / max(
                    1e-9, min(image_box["w"], text_box["w"])
                )
                y_ratio = y_overlap / max(
                    1e-9, min(image_box["h"], text_box["h"])
                )
                horizontal_gap = max(
                    0.0,
                    max(
                        text_box["x"]
                        - (image_box["x"] + image_box["w"]),
                        image_box["x"]
                        - (text_box["x"] + text_box["w"]),
                    ),
                )
                vertical_gap = max(
                    0.0,
                    max(
                        text_box["y"]
                        - (image_box["y"] + image_box["h"]),
                        image_box["y"]
                        - (text_box["y"] + text_box["h"]),
                    ),
                )
                distance = math.hypot(
                    image_center[0] - text_center[0],
                    image_center[1] - text_center[1],
                )
                same_group = bool(
                    image.get("group_id")
                    and image.get("group_id")
                    == text.get("group_id")
                )

                evidence = []
                candidate_type = "nearby"
                score = max(
                    0.05,
                    0.36 * (1.0 - min(1.0, distance)),
                )

                if same_group:
                    evidence.append("same_group")
                    score += 0.22

                if x_ratio >= 0.3 and vertical_gap <= 0.14:
                    if (
                        text_box["y"]
                        >= image_box["y"] + image_box["h"] - 1e-9
                    ):
                        candidate_type = "caption_below"
                        evidence.append(
                            "horizontal_overlap_and_text_below"
                        )
                    elif (
                        image_box["y"]
                        >= text_box["y"] + text_box["h"] - 1e-9
                    ):
                        candidate_type = "caption_above"
                        evidence.append(
                            "horizontal_overlap_and_text_above"
                        )
                    score = max(
                        score,
                        0.54
                        + 0.25 * min(1.0, x_ratio)
                        + 0.15
                        * (
                            1.0
                            - min(1.0, vertical_gap / 0.14)
                        ),
                    )
                elif y_ratio >= 0.3 and horizontal_gap <= 0.18:
                    if (
                        text_box["x"]
                        >= image_box["x"] + image_box["w"] - 1e-9
                    ):
                        candidate_type = "adjacent_right"
                        evidence.append(
                            "vertical_overlap_and_text_right"
                        )
                    else:
                        candidate_type = "adjacent_left"
                        evidence.append(
                            "vertical_overlap_and_text_left"
                        )
                    score = max(
                        score,
                        0.48
                        + 0.24 * min(1.0, y_ratio)
                        + 0.14
                        * (
                            1.0
                            - min(1.0, horizontal_gap / 0.18)
                        ),
                    )

                if x_overlap > 0 and y_overlap > 0:
                    candidate_type = "overlap"
                    evidence.append("bounding_boxes_overlap")
                    score = max(
                        score,
                        0.62
                        + 0.18
                        * min(1.0, (x_ratio + y_ratio) / 2.0),
                    )

                if not evidence:
                    evidence.append("center_distance")

                payload = {
                    "slide_id": slide["slide_id"],
                    "image_element_id": image["element_id"],
                    "text_element_id": text["element_id"],
                    "candidate_type": candidate_type,
                    "evidence": evidence,
                    "metrics": {
                        "x_overlap_ratio": round(x_ratio, 6),
                        "y_overlap_ratio": round(y_ratio, 6),
                        "horizontal_gap": round(horizontal_gap, 6),
                        "vertical_gap": round(vertical_gap, 6),
                        "center_distance": round(distance, 6),
                    },
                }
                serialized = json.dumps(
                    payload,
                    ensure_ascii=False,
                    sort_keys=True,
                    separators=(",", ":"),
                )
                candidates.append(
                    {
                        "candidate_id": (
                            "spatial-"
                            + hashlib.sha256(
                                serialized.encode("utf-8")
                            ).hexdigest()[:24]
                        ),
                        "status": "candidate",
                        "semantic_fact": False,
                        "method": "deterministic_spatial_v1",
                        **payload,
                        "score": round(min(0.99, score), 6),
                    }
                )

            candidates.sort(
                key=lambda item: (
                    -item["score"],
                    item["text_element_id"],
                )
            )
            for candidate in candidates[:3]:
                image["spatial_relation_candidate_ids"].append(
                    candidate["candidate_id"]
                )
                selected_candidates.append(candidate)

        slide["spatial_relation_candidates"] = (
            selected_candidates
        )

    @staticmethod
    def reading_order_key(element):
        bbox = element.get("bbox_normalized") or {}
        x = bbox.get("x")
        y = bbox.get("y")
        role = element.get("role", "")
        role_priority = (
            -2
            if role in {"title", "ctrTitle"}
            else -1
            if role == "subTitle"
            else 0
        )
        if x is None or y is None:
            return (
                role_priority,
                2.0,
                2.0,
                element.get("z_order", 0),
            )
        return (
            role_priority,
            round(y, 5),
            round(x, 5),
            element.get("z_order", 0),
        )

    def assign_reading_order(self, slide):
        ordered = sorted(
            slide["elements"], key=self.reading_order_key
        )
        for index, element in enumerate(ordered, 1):
            element["reading_order"] = index
        slide["reading_order"] = [
            element["element_id"] for element in ordered
        ]

    def parse_slide(
        self,
        number,
        source_slide_id,
        slide_part,
        slide_size,
    ):
        integrity_start = len(self.integrity_errors)
        root = self.read_xml(slide_part, required=True)
        slide_id = f"slide-{number:04d}"
        slide = {
            "slide_id": slide_id,
            "source_slide_id": source_slide_id,
            "number": number,
            "part": slide_part,
            "name": "",
            "hidden": False,
            "size_emu": dict(slide_size),
            "elements": [],
            "groups": [],
            "reading_order": [],
            "spatial_relation_candidates": [],
            "speaker_notes": "",
            "link_ids": [],
            "parse_status": "failed" if root is None else "parsed",
        }
        if root is None:
            slide["integrity_errors"] = list(
                self.integrity_errors[integrity_start:]
            )
            return slide

        c_sld = next(
            (
                node
                for node in root.iter()
                if local_name(node.tag) == "cSld"
            ),
            None,
        )
        if c_sld is not None:
            slide["name"] = c_sld.attrib.get("name", "")

        slide["hidden"] = (
            str(root.attrib.get("show", "1")).lower()
            in {"0", "false", "off"}
        )
        relationships = self.relationships(slide_part)
        fallback_maps = self.placeholder_fallbacks(
            relationships
        )
        shape_tree = next(
            (
                node
                for node in root.iter()
                if local_name(node.tag) == "spTree"
            ),
            None,
        )

        self.current_slide = slide
        self.element_counter = 0
        self.z_counter = 0
        for layer, part in self.inheritance_parts(relationships):
            self.parse_inherited_images(part, layer, slide, slide_size)
        self.current_source_layer = "slide"
        self.current_source_part = slide_part
        self.parse_background(
            root, slide, relationships, slide_size
        )
        if shape_tree is not None:
            self.parse_container(
                shape_tree,
                slide,
                relationships,
                IDENTITY,
                [],
                fallback_maps,
                slide_size,
            )
        else:
            self.warn(f"{slide_id} 没有 p:spTree")
            self.fail(
                "ppt_slide_shape_tree_missing",
                f"{slide_id} 没有 p:spTree",
                slide_id=slide_id,
                source_part=slide_part,
            )

        notes, note_links = self.parse_notes(relationships)
        slide["speaker_notes"] = notes
        self.assign_reading_order(slide)
        self.build_spatial_candidates(slide)

        link_ids = list(note_links)
        for element in slide["elements"]:
            for link_id in element.get("links", []):
                if link_id not in link_ids:
                    link_ids.append(link_id)
            if element["type"] == "table":
                for row in element.get("rows", []):
                    for cell in row.get("cells", []):
                        for link_id in cell.get("links", []):
                            if link_id not in link_ids:
                                link_ids.append(link_id)
        slide["link_ids"] = link_ids
        self.current_slide = None
        self.current_source_part = None
        slide_errors = self.integrity_errors[integrity_start:]
        if slide_errors:
            slide["parse_status"] = "incomplete"
            slide["integrity_errors"] = list(slide_errors)
        return slide

    def presentation_order(self, root, relationships):
        output = []
        slide_list = next(
            (
                node
                for node in root.iter()
                if local_name(node.tag) == "sldIdLst"
            ),
            None,
        )
        if slide_list is not None:
            self.declared_slide_count = sum(
                local_name(node.tag) == "sldId"
                for node in slide_list
            )
            for node in slide_list:
                if local_name(node.tag) != "sldId":
                    continue
                relation_id = node.attrib.get(qn(R, "id"), "")
                relation = relationships.get(relation_id)
                if (
                    relation
                    and str(relation.get("type") or "").endswith("/slide")
                    and relation.get("resolved_target")
                    in self.names
                ):
                    output.append(
                        (
                            node.attrib.get("id", ""),
                            relation["resolved_target"],
                        )
                    )
                else:
                    self.warn(
                        "演示文稿中的幻灯片关系 "
                        f"{relation_id or 'unknown'} 无法解析"
                    )
                    self.fail(
                        "ppt_declared_slide_unresolved",
                        f"演示文稿中的幻灯片关系 {relation_id or 'unknown'} 无法解析",
                        source_part="ppt/presentation.xml",
                        relationship_id=relation_id or None,
                        source_slide_id=node.attrib.get("id") or None,
                    )
            if not output and self.declared_slide_count:
                self.fail(
                    "ppt_all_declared_slides_unresolved",
                    "演示文稿声明的幻灯片均无法解析",
                    declared_slide_count=self.declared_slide_count,
                )
        if output:
            return output

        fallback = []
        for name in self.names:
            match = re.fullmatch(
                r"ppt/slides/slide(\d+)\.xml", name
            )
            if match:
                fallback.append(
                    (int(match.group(1)), name)
                )
        fallback.sort(key=lambda item: item[0])
        if fallback:
            self.warn(
                "未找到 p:sldIdLst，"
                "已按幻灯片数字编号回退排序"
            )
            self.fail(
                "ppt_slide_order_fallback_used",
                "未找到可验证的 p:sldIdLst，无法确认真实幻灯片顺序",
                fallback_slide_count=len(fallback),
            )
            self.declared_slide_count = len(fallback)
        return [
            (str(number), name)
            for number, name in fallback
        ]

    @staticmethod
    def presentation_size(root):
        size = next(
            (
                node
                for node in root.iter()
                if local_name(node.tag) == "sldSz"
            ),
            None,
        )
        if size is None:
            return None
        width = as_int(size.attrib.get("cx"))
        height = as_int(size.attrib.get("cy"))
        if width <= 0 or height <= 0:
            return None
        return {"cx": width, "cy": height}

    def resolve_slide_links(self, slides):
        slide_by_part = {
            slide["part"]: slide["slide_id"]
            for slide in slides
        }
        for link in self.links:
            target = link.get("target", "").split("#", 1)[0]
            if target in slide_by_part:
                link["target_slide_id"] = slide_by_part[target]

    def render_run(self, run, link_map):
        text = run.get("text", "")
        links = run.get("links", [])
        if not text or not links:
            return text
        link = link_map.get(links[0])
        if not link:
            return text
        target = link.get("target", "")
        if link.get("kind") == "external" and valid_http_url(
            target
        ):
            label = text.replace("[", r"\[").replace("]", r"\]")
            return f"[{label}](<{target.replace('>', '%3E')}>)"
        return text

    def render_paragraphs(self, paragraphs, link_map):
        rendered = []
        for paragraph in paragraphs:
            text = "".join(
                self.render_run(run, link_map)
                for run in paragraph.get("runs", [])
            )
            level = max(0, as_int(paragraph.get("level")))
            if level:
                text = f"{'  ' * min(level, 8)}- {text}"
            rendered.append(text)
        return "\n\n".join(rendered).strip()

    def render_table(self, element, link_map):
        rows = []
        width = max(1, as_int(element.get("column_count"), 1))
        for row in element.get("rows", []):
            values = [""] * width
            for cell in row.get("cells", []):
                value = self.render_paragraphs(
                    cell.get("paragraphs", []), link_map
                )
                column = max(0, as_int(cell.get("column")))
                if column >= len(values):
                    values.extend([""] * (column - len(values) + 1))
                values[column] = (
                    value.replace("|", r"\|").replace("\n", "<br>")
                )
            rows.append(values)

        if not rows:
            return ""
        width = max(len(row) for row in rows)
        rows = [
            row + [""] * (width - len(row))
            for row in rows
        ]
        output = [
            "| " + " | ".join(rows[0]) + " |",
            "| "
            + " | ".join("---" for _ in range(width))
            + " |",
        ]
        output.extend(
            "| " + " | ".join(row) + " |"
            for row in rows[1:]
        )
        return "\n".join(output)

    def render_extra_links(
        self, element, link_map, embedded_links
    ):
        lines = []
        for link_id in element.get("links", []):
            if link_id in embedded_links:
                continue
            link = link_map.get(link_id)
            if not link:
                continue
            target = link.get("target", "")
            label = link.get("display_text") or "链接"
            if (
                link.get("kind") == "external"
                and valid_http_url(target)
            ):
                lines.append(
                    f"- {label}: <{target.replace('>', '%3E')}>"
                )
            else:
                lines.append(
                    f"- {label}: "
                    f"{link.get('target_slide_id') or target}"
                )
        return "\n".join(lines)

    def render_markdown(self, structured):
        link_map = {
            link["link_id"]: link
            for link in self.links
        }
        sections = []

        for slide in structured["slides"]:
            heading = f"## 幻灯片 {slide['number']}"
            if slide.get("name"):
                heading += f" · {slide['name']}"
            if slide.get("hidden"):
                heading += "（隐藏）"

            body = [heading]
            by_id = {
                element["element_id"]: element
                for element in slide["elements"]
            }
            for element_id in slide["reading_order"]:
                element = by_id[element_id]
                embedded_links = set()

                if element["type"] in {"text", "shape"}:
                    rendered = self.render_paragraphs(
                        element.get("paragraphs", []),
                        link_map,
                    )
                    for paragraph in element.get(
                        "paragraphs", []
                    ):
                        for run in paragraph.get("runs", []):
                            embedded_links.update(
                                run.get("links", [])
                            )
                    if rendered:
                        body.append(rendered)

                elif element["type"] == "table":
                    rendered = self.render_table(
                        element, link_map
                    )
                    if rendered:
                        body.append(rendered)

                elif element["type"] == "image":
                    asset = self.assets.get(
                        element.get("asset_id")
                    )
                    alt_text = (
                        element.get("alt_text")
                        or element.get("name")
                        or "幻灯片图片"
                    )
                    if asset:
                        if asset.get("attachment_name"):
                            body.append(
                                f"![{alt_text}]"
                                f"(attachment://{element['element_id']})\n"
                                "<!-- yunspire "
                                f"asset_id={asset['asset_id']} "
                                f"element_id={element['element_id']} "
                                "-->"
                            )
                        else:
                            failure = asset.get("localization") or {}
                            body.append(
                                "[外链图片本地化失败："
                                f"{alt_text}；"
                                f"{failure.get('message') or '无法读取'}]"
                            )
                    elif element.get("external_source"):
                        failure = element.get(
                            "external_image_localization"
                        ) or {}
                        body.append(
                            "[外链图片本地化失败："
                            f"{alt_text}；"
                            f"{failure.get('code') or 'unresolved'}；"
                            f"{failure.get('message') or '无法读取'}]"
                        )
                    else:
                        body.append(
                            "[未解析图片："
                            f"{element['element_id']}]"
                        )

                extra_links = self.render_extra_links(
                    element, link_map, embedded_links
                )
                if extra_links:
                    body.append(extra_links)

            if slide.get("speaker_notes"):
                body.append(
                    "### 演讲者备注\n\n"
                    + slide["speaker_notes"]
                )
            sections.append(
                "\n\n".join(
                    part for part in body if part
                )
            )

        return "\n\n".join(sections)

    def attachments_output(self):
        output = []
        for asset_id, metadata in self.assets.items():
            data = self.asset_bytes.get(asset_id)
            if data is not None:
                payload = {
                    "asset_id": asset_id,
                    "name": metadata["attachment_name"],
                    "original_names": list(
                        metadata["original_names"]
                    ),
                    "package_parts": list(
                        metadata["package_parts"]
                    ),
                    "size": len(data),
                    "mime_type": metadata["mime_type"],
                    "sha256": metadata["sha256"],
                    "source_part": (
                        metadata["package_parts"][0]
                        if metadata["package_parts"]
                        else None
                    ),
                    "references": list(metadata["references"]),
                    "data_base64": base64.b64encode(
                        data
                    ).decode("ascii"),
                }
            else:
                payload = self.external_images.attachment_payload(metadata)
                if payload is None:
                    continue
                payload.update(
                    {
                        "original_names": list(
                            metadata.get("original_names", [])
                        ),
                        "package_parts": [],
                        "source_part": metadata.get("requested_url"),
                        "source_url": metadata.get("requested_url"),
                        "references": list(metadata["references"]),
                    }
                )
            output.append(payload)
        return output

    def extract(self):
        source = {
            "file_name": self.path.name,
            "path": str(self.path),
            "byte_length": self.path.stat().st_size,
            "sha256": stream_sha256(self.path),
        }

        with zipfile.ZipFile(self.path) as archive:
            self.archive = archive
            self.names = set(archive.namelist())
            self.validate_paths()
            self.load_content_types()

            presentation_part = "ppt/presentation.xml"
            presentation = self.read_xml(
                presentation_part, required=True
            )
            if presentation is None:
                raise ValueError("PPTX 缺少或损坏 ppt/presentation.xml")

            slide_size = self.presentation_size(
                presentation
            )
            if slide_size is None:
                slide_size = {
                    "cx": 9_144_000,
                    "cy": 6_858_000,
                }
                self.warn(
                    "演示文稿没有有效 p:sldSz，"
                    "已使用标准页面作为坐标回退"
                )
                self.fail(
                    "ppt_slide_size_missing",
                    "演示文稿没有有效 p:sldSz，无法确认原始页面尺寸",
                    source_part=presentation_part,
                )

            order = self.presentation_order(
                presentation,
                self.relationships(presentation_part),
            )
            if not order:
                self.fail(
                    "ppt_no_declared_slides",
                    "PPTX 未包含可解析的幻灯片",
                    source_part=presentation_part,
                )
                order = []
            slides = [
                self.parse_slide(
                    index,
                    source_slide_id,
                    part,
                    slide_size,
                )
                for index, (source_slide_id, part)
                in enumerate(order, 1)
            ]
            if not any(
                slide.get("parse_status") == "parsed"
                for slide in slides
            ):
                self.fail(
                    "ppt_all_slides_incomplete",
                    "PPTX 的全部声明幻灯片均未完成解析",
                    declared_slide_count=getattr(
                        self, "declared_slide_count", len(order)
                    ),
                )
            self.resolve_slide_links(slides)

            structured = {
                "format": "yunspire.office-document.v2",
                "document_type": "presentation",
                "source": source,
                "slide_size_emu": slide_size,
                "slide_count": len(slides),
                "slides": slides,
                "integrity": self.integrity_output(
                    {
                        "declared_slide_count": getattr(
                            self, "declared_slide_count", len(order)
                        ),
                        "returned_slide_count": len(slides),
                        "parsed_slide_count": sum(
                            slide.get("parse_status") == "parsed"
                            for slide in slides
                        ),
                        "incomplete_slide_count": sum(
                            slide.get("parse_status") == "incomplete"
                            for slide in slides
                        ),
                        "failed_slide_count": sum(
                            slide.get("parse_status") == "failed"
                            for slide in slides
                        ),
                        "resolved_image_count": sum(
                            bool(element.get("asset_id"))
                            for slide in slides
                            for element in slide.get("elements", [])
                            if element.get("type") == "image"
                        ),
                    }
                ),
                "assets": self.public_assets(),
                "links": self.links,
                "semantics": {
                    "spatial_relations_are_candidates": True,
                    "spatial_relations_are_semantic_facts": False,
                    "candidate_method": (
                        "deterministic_spatial_v1"
                    ),
                },
                "external_image_localization": localization_summary(
                    [
                        asset
                        for asset in self.public_assets()
                        if asset.get("localization")
                    ]
                ),
                "security": {
                    "content_role": "untrusted_data",
                    "instruction_authority": False,
                    "tool_authority": False,
                },
            }
            return (
                self.render_markdown(structured),
                structured,
                self.attachments_output(),
                self.warnings,
            )

    @staticmethod
    def empty_structured(source):
        return {
            "format": "yunspire.office-document.v2",
            "document_type": "presentation",
            "source": source,
            "slide_size_emu": {
                "cx": None,
                "cy": None,
            },
            "slide_count": 0,
            "slides": [],
            "assets": [],
            "links": [],
            "semantics": {
                "spatial_relations_are_candidates": True,
                "spatial_relations_are_semantic_facts": False,
            },
            "security": {
                "content_role": "untrusted_data",
                "instruction_authority": False,
                "tool_authority": False,
            },
        }


def extract_pptx(path, external_asset_directory=None):
    """Return markdown, structured data, attachments, and warnings.

    This parser applies no size-based truncation. Resource budgeting and model
    batching belong to the caller, after deterministic extraction is complete.
    """
    extractor = PptxExtractor(path, external_asset_directory)
    try:
        return extractor.extract()
    finally:
        extractor.close()


__all__ = ["extract_pptx"]
