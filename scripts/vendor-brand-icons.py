#!/usr/bin/env python3
"""Vendor the complete pinned SVGL catalog. No npm install or executable SVG.

Reproduce offline with --archive /path/to/the/pinned/archive.tar.gz. Updating
upstream requires reviewing both pins and replacing the official catalog file.
"""
import argparse
import base64
import hashlib
import io
import json
from pathlib import Path
import re
import struct
import subprocess
import tarfile
import urllib.parse
import urllib.request
import xml.etree.ElementTree as ET

REVISION = "f9b726cee1d1f3c697a18c880185e89d9a55bb70"
ARCHIVE_SHA256 = "413ad5a158fd62555d40c9556d6add38913e01fc5f6baffe512e77ebd9f5e574"
CATALOG_SHA256 = "ed59ee926d621300d6a322c73ab79ab8845b6e70bae4f7a28094561b9991f02a"
ARCHIVE_URL = f"https://codeload.github.com/pheralb/svgl/tar.gz/{REVISION}"
ROOT = Path(__file__).resolve().parent.parent
ASSETS = ROOT / "apps/agents-window/assets/brands/svgl"
GENERATED = ROOT / "apps/agents-window/src/brand_icons_catalog.rs"
MAX_SVG = 21 * 1024
SVG_NS = "http://www.w3.org/2000/svg"
XLINK_NS = "http://www.w3.org/1999/xlink"
ET.register_namespace("", SVG_NS)
ET.register_namespace("xlink", XLINK_NS)
VECTOR_TAGS = set("svg g path rect circle ellipse polygon polyline line defs clipPath mask linearGradient radialGradient stop filter feGaussianBlur feColorMatrix feBlend feComposite feFlood feOffset feMerge feMergeNode pattern use symbol".split())
OMIT_TAGS = set("script metadata title desc namedview guide sfw slices sliceSourceBounds RDF Work format type description".split())
RASTER_TAGS = {"image", "text", "tspan"}


def sha(data):
    return hashlib.sha256(data).hexdigest()


def read_bounded(path, limit):
    with path.open("rb") as file:
        data = file.read(limit + 1)
    if len(data) > limit:
        raise ValueError("Source exceeds its size limit")
    return data


def download_archive():
    # Only the pinned public origin is contacted; never an MCP or brand URL.
    request = urllib.request.Request(ARCHIVE_URL, headers={"User-Agent": "openADE-icon-vendor"})
    opener = urllib.request.build_opener(NoRedirect())
    with opener.open(request, timeout=30) as response:
        data = response.read(20 * 1024 * 1024 + 1)
    if len(data) > 20 * 1024 * 1024:
        raise ValueError("Archive is too large")
    return data


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise ValueError("Unexpected redirect from the pinned archive origin")


def route_path(route):
    parsed = urllib.parse.urlsplit(route)
    if (parsed.scheme != "https" or parsed.netloc != "svgl.app" or parsed.query or parsed.fragment
            or not re.fullmatch(r"/library/[A-Za-z0-9_. -]+\.svg", parsed.path)):
        raise ValueError("Unexpected SVG origin/path")
    return parsed.path


def safe_css(text):
    if "@" in text or "\\" in text or re.search(r"expression\s*\(", text, re.I):
        raise ValueError("Active or escaped CSS is not supported")
    for match in re.finditer(r"url\s*\((.*?)\)", text, re.I):
        reference = match.group(1).strip().strip("\"'")
        if not reference.startswith("#") or any(c in reference for c in "\r\n<>"):
            raise ValueError("SVG references must stay inside the document")


def sanitize(data):
    if len(data) > MAX_SVG:
        raise ValueError("SVG exceeds upstream's 21 KiB bound")
    text = data.decode("utf-8-sig")
    if re.search(r"<!ENTITY|<!DOCTYPE[^>]*\[", text, re.I):
        raise ValueError("Entity declarations are not allowed")
    # Some original exports declare an external SVG DTD. It is not fetched.
    text = re.sub(r"<!DOCTYPE[^>]*>", "", text, flags=re.I)
    root = ET.fromstring(text)
    if root.tag.split("}")[-1] != "svg":
        raise ValueError("Expected an SVG root")
    needs_raster = False

    def clean(node):
        nonlocal needs_raster
        for child in list(node):
            tag = child.tag.split("}")[-1]
            if tag in OMIT_TAGS:
                node.remove(child)
                continue
            if tag == "style":
                # Theme selection is explicit in the catalog. Browser media
                # rules are not interpreted by the native vector renderer.
                child.text = re.sub(r"@media[^{}]*\{(?:[^{}]|\{[^{}]*\})*\}", "", child.text or "")
                safe_css(child.text or "")
            elif tag in RASTER_TAGS:
                needs_raster = True
            elif tag not in VECTOR_TAGS:
                raise ValueError(f"Unsupported SVG element: {tag}")
            clean(child)
        for key, value in list(node.attrib.items()):
            local = key.split("}")[-1]
            if local.lower().startswith("on"):
                del node.attrib[key]
            elif local in {"href", "src"}:
                if value.startswith("#"):
                    continue
                if (node.tag.split("}")[-1] == "image" and value.startswith("data:image/png;base64,")):
                    image = base64.b64decode(value.split(",", 1)[1], validate=True)
                    if len(image) > MAX_SVG or not image.startswith(b"\x89PNG\r\n\x1a\n"):
                        raise ValueError("Invalid embedded PNG")
                    if len(image) < 24 or max(struct.unpack(">II", image[16:24])) > 4096:
                        raise ValueError("Embedded PNG dimensions exceed the vendor limit")
                    continue
                raise ValueError("External SVG resource")
            else:
                safe_css(value)

    clean(root)
    root.set("preserveAspectRatio", "xMidYMid meet")
    result = ET.tostring(root, encoding="unicode") + "\n"
    if len(result.encode()) > 64 * 1024:
        raise ValueError("Sanitized SVG exceeds its size limit")
    return result, needs_raster


def raster(source):
    # Five upstream files contain embedded PNGs or font glyphs that the native
    # vector-only renderer cannot draw. Flatten only these during vendoring.
    document = ET.fromstring(source)
    if "viewBox" not in document.attrib:
        raise ValueError("Raster fallback requires an explicit viewBox")
    document.set("width", "64")
    document.set("height", "64")
    source = ET.tostring(document, encoding="unicode")
    command = ["magick", "-limit", "memory", "64MiB", "-limit", "map", "64MiB",
               "-limit", "disk", "0", "-limit", "width", "4096", "-limit", "height", "4096",
               "-limit", "thread", "1", "-background", "none", "svg:-", "-resize", "64x64",
               "-gravity", "center", "-extent", "64x64", "-strip", "PNG32:-"]
    result = subprocess.run(command, input=source.encode(), stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, timeout=10, check=False)
    if result.returncode or not result.stdout.startswith(b"\x89PNG\r\n\x1a\n") or len(result.stdout) > 64 * 1024:
        raise ValueError("Could not safely rasterize an upstream image/text logo")
    return result.stdout


def rust(text):
    return json.dumps(text, ensure_ascii=False)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archive", type=Path)
    parser.add_argument("--check", action="store_true", help="validate existing assets and generated source without changing them")
    args = parser.parse_args()
    catalog_bytes = read_bounded(ASSETS / "catalog.json", 256 * 1024)
    if sha(catalog_bytes) != CATALOG_SHA256:
        raise ValueError("Official catalog pin differs; review the update explicitly")
    catalog = json.loads(catalog_bytes)
    if len(catalog) != 670:
        raise ValueError("Unexpected catalog length")
    archive_bytes = read_bounded(args.archive, 20 * 1024 * 1024) if args.archive else download_archive()
    if sha(archive_bytes) != ARCHIVE_SHA256:
        raise ValueError("Pinned archive digest differs")
    outputs = {}
    records = []
    artwork = {}
    with tarfile.open(fileobj=io.BytesIO(archive_bytes), mode="r:gz") as archive:
        for entry in catalog:
            routes = entry["route"]
            routes = routes if isinstance(routes, dict) else {"light": routes, "dark": routes}
            if set(routes) != {"light", "dark"}:
                raise ValueError("Missing theme variant")
            for route in routes.values():
                if route in artwork:
                    continue
                path = route_path(route)
                member = archive.getmember(f"svgl-{REVISION}/static{path}")
                if not member.isfile() or member.size > MAX_SVG:
                    raise ValueError("Unbounded/non-regular SVG member")
                original = archive.extractfile(member).read(MAX_SVG + 1)
                source, needs_raster = sanitize(original)
                variants = {}
                for theme, color in [("light", "#000000"), ("dark", "#FFFFFF")]:
                    themed = source.replace("currentColor", color)
                    extension = "png" if needs_raster else "svg"
                    data = raster(themed) if needs_raster else themed.encode()
                    filename = f"{sha(data)}.{extension}"
                    outputs[filename] = data
                    variants[theme] = filename
                artwork[route] = variants
                records.append({"url": route, "original_sha256": sha(original),
                                "adaptation": "sanitized SVG; currentColor resolved per theme" + ("; image/text flattened to PNG64" if needs_raster else ""),
                                **{theme: {"file": variants[theme], "sha256": variants[theme].split('.')[0]} for theme in ["light", "dark"]}})
            entry["_routes"] = routes
    groups = {}
    for entry in catalog:
        key = json.dumps(entry["_routes"], sort_keys=True, separators=(",", ":"))
        if key not in groups:
            groups[key] = {"entry": entry, "aliases": [], "categories": set()}
        group = groups[key]
        group["aliases"].append(entry["title"])
        category = entry["category"]
        group["categories"].update(category if isinstance(category, list) else [category])
    generated = ["// Generated by scripts/vendor-brand-icons.py. Do not edit by hand.\n", "use super::{BrandArtwork, BrandEntry};\n", "pub(super) static ENTRIES: &[BrandEntry] = &[\n"]
    ids = set()
    for key, group in sorted(groups.items(), key=lambda pair: (pair[1]["entry"]["title"].casefold(), pair[0])):
        entry = group["entry"]
        identity = "svgl-" + sha(key.encode())[:24]
        if identity in ids:
            raise ValueError("Duplicate stable identity")
        ids.add(identity)
        def art(theme):
            file = artwork[entry["_routes"][theme]][theme]
            path = f"../assets/brands/svgl/artwork/{file}"
            if file.endswith(".png"):
                return f"BrandArtwork::Png(include_bytes!({rust(path)}))"
            return f"BrandArtwork::Svg(include_str!({rust(path)}))"
        generated.append("    BrandEntry { id: " + rust(identity) + ", title: " + rust(entry["title"]) + ", website: " + rust(entry["url"]) + ", aliases: &[" + ", ".join(rust(value) for value in sorted(set(group["aliases"]))) + "], categories: &[" + ", ".join(rust(value) for value in sorted(group["categories"])) + "], light: " + art("light") + ", dark: " + art("dark") + " },\n")
    generated.append("];\n")
    manifest = {"source": "SVGL", "repository": "https://github.com/pheralb/svgl", "revision": REVISION,
                "api": "https://svgl.app/docs/api", "catalog_url": "https://api.svgl.app", "catalog_sha256": CATALOG_SHA256,
                "archive_url": ARCHIVE_URL, "archive_sha256": ARCHIVE_SHA256, "license": "MIT", "license_file": "LICENSE",
                "upstream_entries": len(catalog), "selectable_artworks": len(groups), "unique_upstream_svg": len(records),
                "rasterizer": "ImageMagick 7.1.2-24 Q16-HDRI + librsvg 2.62.1 (PNG64 only for embedded raster/text)", "assets": sorted(records, key=lambda record: record["url"])}
    outputs = {ASSETS / "artwork" / name: value for name, value in outputs.items()}
    outputs[GENERATED] = "".join(generated).encode()
    outputs[ASSETS / "SOURCE.json"] = (json.dumps(manifest, ensure_ascii=False, indent=2) + "\n").encode()
    outputs[ASSETS / "LICENSE"] = archive_license(archive_bytes)
    artwork_root = ASSETS / "artwork"
    stale = [path for path in artwork_root.glob("*") if re.fullmatch(r"[0-9a-f]{64}\.(svg|png)", path.name) and path not in outputs]
    if args.check:
        if stale:
            raise ValueError("Orphaned generated artwork; regenerate the vendor snapshot")
        for path, data in outputs.items():
            if not path.is_file() or path.read_bytes() != data:
                raise ValueError(f"Generated asset differs: {path.relative_to(ROOT)}")
    else:
        for path in stale:
            path.unlink()
        for path, data in outputs.items():
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
    print(f"{len(catalog)} upstream entries, {len(groups)} selectable artworks, {len(records)} source SVGs; {len(outputs)} validated outputs")


def archive_license(data):
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as archive:
        return archive.extractfile(f"svgl-{REVISION}/LICENSE").read(16 * 1024)


if __name__ == "__main__":
    main()
