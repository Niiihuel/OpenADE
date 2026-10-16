#!/usr/bin/env python3
"""Security regressions for the build-time SVGL sanitizer; no network needed."""
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("vendor", Path(__file__).with_name("vendor-brand-icons.py"))
vendor = importlib.util.module_from_spec(spec)
spec.loader.exec_module(vendor)


class BrandSvgSanitizerTests(unittest.TestCase):
    def test_removes_active_nodes_and_handlers_but_preserves_color_and_internal_references(self):
        source, raster = vendor.sanitize(b'<svg xmlns="http://www.w3.org/2000/svg" onload="alert(1)"><script/><defs><linearGradient id="a"/></defs><path fill="url(#a)" d="M0 0h10v10z"/><circle fill="#ff0000" r="1"/></svg>')
        self.assertFalse(raster)
        self.assertNotIn("script", source)
        self.assertNotIn("onload", source)
        self.assertIn("#ff0000", source)
        self.assertIn("url(#a)", source)

    def test_external_resources_css_and_entity_expansion_are_rejected(self):
        for source in [
            '<svg><image href="file:///private"/></svg>',
            '<svg><use href="https://example.test/logo.svg"/></svg>',
            '<svg><style>@import url(https://example.test)</style></svg>',
            '<svg><path fill="url(https://example.test)"/></svg>',
            '<svg><style>.x{fill:uRl("https://example.test")}</style></svg>',
            '<svg><style>.x{fill:url(\\68ttps://example.test)}</style></svg>',
            '<!DOCTYPE svg [<!ENTITY x "expanded">]><svg>&x;</svg>',
            '<svg><foreignObject/></svg>',
            '<svg><image href="data:image/svg+xml;base64,PHN2Zy8+"/></svg>',
        ]:
            with self.subTest(source=source):
                with self.assertRaises(ValueError):
                    vendor.sanitize(source.encode())

    def test_external_dtd_is_removed_without_fetch_and_output_is_bounded(self):
        source, _ = vendor.sanitize(b'<!DOCTYPE svg PUBLIC "unused" "https://example.test/not-fetched"><svg/>')
        self.assertNotIn("DOCTYPE", source)
        with self.assertRaises(ValueError):
            vendor.sanitize(b"x" * (vendor.MAX_SVG + 1))

    def test_only_public_official_svg_paths_are_accepted(self):
        self.assertEqual(vendor.route_path("https://svgl.app/library/sample-dark.svg"), "/library/sample-dark.svg")
        for url in ["http://svgl.app/library/a.svg", "https://svgl.app.evil.test/library/a.svg", "https://user@svgl.app/library/a.svg", "https://svgl.app/library/../a.svg", "https://svgl.app/library/a.svg?secret=value"]:
            with self.subTest(url=url):
                with self.assertRaises(ValueError):
                    vendor.route_path(url)


if __name__ == "__main__":
    unittest.main()
