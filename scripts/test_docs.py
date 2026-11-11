"""Focused checks for source staging, navigation, and link validation."""

from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import docs


class DocumentationTests(unittest.TestCase):
    def test_published_paths_map_indexes_and_landing_page(self):
        self.assertEqual(docs.published_path(Path("docs/README.md")), Path("docs/index.md"))
        self.assertEqual(docs.published_path(Path("docs/introduction.md")), Path("index.md"))
        self.assertEqual(docs.published_path(Path("docs/frames.md")), Path("docs/frames.md"))

    def test_links_keep_published_navigation_relative(self):
        page = Path("docs/introduction.md")
        self.assertEqual(docs.repository_link("README.md#configuration", page),
                         "docs/index.md#configuration")
        self.assertEqual(docs.repository_link("../architecture/README.md", page),
                         "architecture/index.md")
        self.assertEqual(docs.repository_link("benchmarks/silesia-2026-11-10/after.csv", page),
                         "docs/benchmarks/silesia-2026-11-10/after.csv")

    def test_source_links_point_at_repository(self):
        self.assertEqual(docs.repository_link("../Cargo.toml", Path("docs/README.md")),
                         f"{docs.REPOSITORY}/blob/main/Cargo.toml")
        self.assertEqual(docs.repository_link("../src/", Path("docs/README.md")),
                         f"{docs.REPOSITORY}/tree/main/src")

    def test_external_urls_and_fragments_are_preserved(self):
        for url in ["https://docs.rs/mopenzl", "#configuration", "mailto:test@example.com"]:
            self.assertEqual(docs.repository_link(url, Path("docs/README.md")), url)

    def test_missing_source_destination_fails(self):
        with self.assertRaisesRegex(ValueError, "missing link target"):
            docs.repository_link("missing-page.md", Path("docs/README.md"))

    def test_code_fences_are_not_rewritten(self):
        source = "```md\n[example](missing-page.md)\n```\n[guide](README.md)\n"
        self.assertEqual(docs.rewrite_links(source, Path("docs/frames.md")),
                         "```md\n[example](missing-page.md)\n```\n[guide](index.md)\n")

    def test_reference_definitions_and_html_links_are_rewritten(self):
        source = '[guide]: README.md#configuration\n<a href="README.md">Guide</a>\n'
        expected = '[guide]: index.md#configuration\n<a href="index.md">Guide</a>\n'
        self.assertEqual(docs.rewrite_links(source, Path("docs/frames.md")), expected)

    def test_summary_covers_the_repository(self):
        summary = docs.summary()
        self.assertIn("- [Overview](index.md)", summary)
        self.assertIn("[Getting started](docs/getting-started.md)", summary)
        self.assertIn("[Choose your API](docs/index.md)", summary)
        self.assertIn("[Documentation publishing](architecture/documentation.md)", summary)
        self.assertNotIn("Module map", summary)

    def test_summary_orders_parts_for_reading(self):
        summary = docs.summary()
        parts = [line[2:] for line in summary.splitlines() if line.startswith("# ")]
        self.assertEqual(parts, ["Summary", "Start here", "Graphs and codecs", "Guides", "Performance and correctness",
                                 "How it works", "Contributing"])

    def test_summary_nests_detail_pages_under_their_parent(self):
        summary = docs.summary()
        self.assertIn("- [Streaming and completion](docs/streaming.md)\n"
                      "  - [Read / Write I/O](docs/io.md)", summary)
        self.assertIn("- [Framing containers](docs/frames.md)\n"
                      "  - [Framed resources with independent graphs](docs/framing.md)", summary)
        self.assertIn("    - [zstd](docs/benchmarks/encoders/zstd.md)", summary)
        self.assertIn("    - [zstd](docs/benchmarks/decoders/zstd.md)", summary)
        self.assertLess(summary.index("(docs/getting-started.md)"), summary.index("(docs/graphs.md)"))
        self.assertLess(summary.index("(docs/graphs.md)"), summary.index("(docs/configuration.md)"))
        self.assertIn("- [Compression](architecture/sections/compression.md)\n"
                      "  - [Compressor](architecture/compressor.md)", summary)

    def test_architecture_sections_follow_the_index(self):
        sections = {heading: pages for heading, _, pages in docs.architecture_sections()}
        self.assertNotIn("Module map", sections)
        self.assertNotIn("Where to start", sections)
        self.assertNotIn("Known gaps (whole crate)", sections)
        self.assertIn(("architecture/framing.md", "Framed containers"), sections["Containers"])
        self.assertEqual(docs.section_path("Codec families"),
                         Path("architecture/sections/codec-families.md"))

    def test_section_pages_link_from_their_own_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            with patch.object(docs, "SOURCE", Path(directory)):
                docs.write_section_pages()
            page = (Path(directory) / "architecture/sections/containers.md").read_text()
        self.assertTrue(page.startswith("# Containers\n"))
        self.assertIn("[Framed containers](../framing.md)", page)
        self.assertIn("(../index.md)", page)

    def test_republished_text_links_relative_to_its_new_location(self):
        index = Path("architecture/README.md")
        self.assertEqual(docs.repository_link("../docs/README.md", index,
                                              Path("architecture/sections/x.md")),
                         "../../docs/index.md")

    def test_html_checker_accepts_downloads_fragments_and_pages_prefix(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            (output / "index.html").write_text('<a href="/mopenzl/#welcome">Home</a>'
                                               '<h1 id="welcome">Welcome</h1>'
                                               '<a href="measurements.csv">Data</a>')
            (output / "measurements.csv").write_text("quality,size\n5,10\n")
            (output / "README.md").write_text("# No site links\n")
            with patch.object(docs, "OUTPUT", output), patch.object(docs, "ROOT", output):
                docs.check_links()

    def test_readme_site_links_are_checked_against_the_book(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            output = root / "book"
            (output / "docs").mkdir(parents=True)
            (output / "index.html").write_text("<h1>Home</h1>")
            (output / "docs/guide.html").write_text('<h2 id="setup">Setup</h2>')
            readme = root / "README.md"
            with patch.object(docs, "OUTPUT", output), patch.object(docs, "ROOT", root):
                readme.write_text(f"[site]({docs.SITE}) [guide]: {docs.SITE}docs/guide.html#setup\n")
                docs.check_links()
                readme.write_text(f"[a]({docs.SITE}docs/gone.html) [b]({docs.SITE}docs/guide.html#gone)\n")
                with self.assertRaises(ValueError) as caught:
                    docs.check_links()
            self.assertIn("README.md: missing https", str(caught.exception))
            self.assertIn("README.md: missing anchor", str(caught.exception))

    def test_html_checker_rejects_missing_file_and_anchor(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            (output / "index.html").write_text('<a href="#missing">Anchor</a>'
                                               '<img src="absent.svg">')
            (output / "README.md").write_text("# No site links\n")
            with patch.object(docs, "OUTPUT", output), patch.object(docs, "ROOT", output):
                with self.assertRaises(ValueError) as caught:
                    docs.check_links()
            self.assertIn("missing anchor #missing", str(caught.exception))
            self.assertIn("missing absent.svg", str(caught.exception))


    def test_graph_chapters_derive_values_from_matching_measurements(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            data = root / "docs/benchmarks/silesia-2026-11-10/after.csv"
            data.parent.mkdir(parents=True)
            header = "case,file,bytes,compressed,side,op,median_ns\n"
            rows = "".join(f"zstd,text,1000,100,{side},{op},{time}\n"
                           for side, time in [("c", 2000), ("rust", 1000)]
                           for op in ["compress", "decompress"])
            data.write_text(header + rows)
            with patch.object(docs, "ROOT", root):
                pages = docs.benchmark_pages()
            self.assertEqual(len(pages), 2)
            self.assertIn("| text | 1,000 | 100 | 10.000 | 500.0 | 1,000.0 | 2.00× |",
                          pages["docs/benchmarks/encoders/zstd.md"])
            data.write_text(header + rows + "zstd,text,2000,100,c,compress,2000\n")
            with patch.object(docs, "ROOT", root):
                with self.assertRaisesRegex(ValueError, "mismatched sizes"):
                    docs.benchmark_pages()

    def test_no_measurements_add_no_generated_chapters(self):
        with tempfile.TemporaryDirectory() as directory:
            with patch.object(docs, "ROOT", Path(directory)):
                self.assertEqual(docs.benchmark_pages(), {})


if __name__ == "__main__":
    unittest.main()
