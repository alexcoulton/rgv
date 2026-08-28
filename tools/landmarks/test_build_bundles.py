import gzip
import hashlib
import tempfile
import unittest
from pathlib import Path

import build_bundles


class LandmarkBundleTests(unittest.TestCase):
    @staticmethod
    def digest(path: Path) -> str:
        return hashlib.sha256(path.read_bytes()).hexdigest()

    def test_refgene_normalization_accepts_ucsc_bin_column(self):
        row = "585\tNM_1\tchr1\t+\t10\t50\t20\t40\t2\t10,30,\t20,50,\t0\tGENE\tcmpl\tcmpl\t0,1,"
        fields = build_bundles.normalized_refgene_fields(row)
        self.assertEqual(fields[0], "NM_1")
        self.assertEqual(fields[11], "GENE")
        self.assertEqual(build_bundles.validate_refgene(fields, {"chr1": 100}), (2, 30))

    def test_curator_selects_one_stable_representative_per_gene(self):
        rows = [
            "0\tNR_1\tchr1\t+\t0\t90\t90\t90\t1\t0,\t90,\t0\tA\tnone\tnone\t-1,\n",
            "0\tNM_2\tchr1\t+\t0\t80\t10\t70\t2\t0,40,\t20,80,\t0\tA\tcmpl\tcmpl\t0,1,\n",
            "0\tNM_3\tchr1\t-\t100\t180\t110\t170\t1\t100,\t180,\t0\tB\tcmpl\tcmpl\t0,\n",
        ]
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "source.gz"
            with gzip.open(source, "wt", encoding="utf-8") as stream:
                stream.writelines(rows)
            counts = build_bundles.curate_refgene(
                source,
                root / "genes.genePred.gz",
                root / "names.tsv.gz",
                {"chr1": 200},
                ["chr1"],
            )
            with gzip.open(root / "genes.genePred.gz", "rt", encoding="utf-8") as stream:
                output = [line.split("\t", 1)[0] for line in stream]
            self.assertEqual(output, ["NM_2", "NM_3"])
            self.assertEqual(
                counts,
                {
                    "genes": 2,
                    "source_transcripts": 3,
                    "transcripts": 3,
                    "off_reference_transcripts_excluded": 0,
                    "exons": 4,
                },
            )

    def test_representative_selection_is_byte_deterministic_under_reordered_input(self):
        rows = [
            "0\tNR_1\tchr1\t+\t0\t90\t90\t90\t1\t0,\t90,\t0\tA\tnone\tnone\t-1,\n",
            "0\tNM_2\tchr1\t+\t0\t80\t10\t70\t2\t0,40,\t20,80,\t0\tA\tcmpl\tcmpl\t0,1,\n",
            "0\tNM_3\tchr1\t-\t100\t180\t110\t170\t1\t100,\t180,\t0\tB\tcmpl\tcmpl\t0,\n",
        ]
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            output_hashes = []
            for index, source_rows in enumerate((rows, list(reversed(rows)))):
                source = root / f"source-{index}.gz"
                with gzip.open(source, "wt", encoding="utf-8") as stream:
                    stream.writelines(source_rows)
                genes = root / f"genes-{index}.gz"
                names = root / f"names-{index}.gz"
                build_bundles.curate_refgene(
                    source, genes, names, {"chr1": 200}, ["chr1"]
                )
                output_hashes.append((self.digest(genes), self.digest(names)))
            self.assertEqual(output_hashes[0], output_hashes[1])

    def test_curated_gene_assets_require_matching_symbol_identity_and_coordinates(self):
        representative_row = (
            "NM_2\tchr1\t+\t0\t80\t10\t70\t2\t0,40,\t20,80,\t0\tACTB\tcmpl\tcmpl\t0,1,\n"
        )
        search_row = "actb\tACTB\t7157\tNM_2\tchr1\t0\t80\t+\tbeta actin\n"
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            representatives = root / "genes.genePred.gz"
            names = root / "names.tsv.gz"
            build_bundles.write_gzip_lines(representatives, [representative_row])
            build_bundles.write_gzip_lines(
                names,
                [build_bundles.SEARCH_SCHEMA_VERSION + "\n", search_row],
            )
            self.assertEqual(
                build_bundles.validate_curated_gene_assets(representatives, names), 1
            )

            build_bundles.write_gzip_lines(
                names,
                [
                    build_bundles.SEARCH_SCHEMA_VERSION + "\n",
                    search_row.replace("\t80\t", "\t79\t"),
                ],
            )
            with self.assertRaisesRegex(ValueError, "metadata disagree"):
                build_bundles.validate_curated_gene_assets(representatives, names)

    def test_curated_gene_assets_require_nonempty_stable_gene_id(self):
        representative_row = (
            "NM_2\tchr1\t+\t0\t80\t10\t70\t1\t0,\t80,\t0\tACTB\tcmpl\tcmpl\t0,\n"
        )
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            representatives = root / "genes.genePred.gz"
            names = root / "names.tsv.gz"
            build_bundles.write_gzip_lines(representatives, [representative_row])
            build_bundles.write_gzip_lines(
                names,
                [
                    build_bundles.SEARCH_SCHEMA_VERSION + "\n",
                    "actb\tACTB\t\tNM_2\tchr1\t0\t80\t+\t\n",
                ],
            )
            with self.assertRaisesRegex(ValueError, "incomplete identity"):
                build_bundles.validate_curated_gene_assets(representatives, names)

    def test_off_reference_transcripts_are_excluded_and_counted(self):
        rows = [
            "0\tNM_1\tchr1\t+\t0\t80\t10\t70\t1\t0,\t80,\t0\tA\tcmpl\tcmpl\t0,\n",
            "0\tNM_ALT\tchrUn\t+\t0\t80\t10\t70\t1\t0,\t80,\t0\tALT\tcmpl\tcmpl\t0,\n",
        ]
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "source.gz"
            with gzip.open(source, "wt", encoding="utf-8") as stream:
                stream.writelines(rows)
            counts = build_bundles.curate_refgene(
                source, root / "genes.gz", root / "names.gz", {"chr1": 100}, ["chr1"]
            )
            self.assertEqual(counts["genes"], 1)
            self.assertEqual(counts["off_reference_transcripts_excluded"], 1)

    def test_alias_conflicts_and_cytoband_bounds_are_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            aliases = root / "aliases.tsv"
            aliases.write_text("chr1\t1\nchr2\t1\n", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "targets both"):
                build_bundles.normalize_aliases(
                    aliases,
                    root / "normalized.tsv",
                    {"chr1": 100, "chr2": 100},
                )
            with self.assertRaisesRegex(ValueError, "outside the assembly"):
                build_bundles.validate_cytoband_row(
                    "chr1", 0, 101, "p1", "gneg", {"chr1": 100}
                )

    def test_runtime_manifest_validation_rejects_unsafe_paths_and_missing_licence(self):
        def entry(role: str) -> dict[str, object]:
            return {
                "role": role,
                "path": f"hg38/{role}.dat",
                "format": "test",
                "bytes": 1,
                "sha256": "a" * 64,
                "source_url": "https://example.test/source",
                "upstream_project": "fixture",
                "upstream_version": "1",
                "derivation": "fixture",
                "citation": "fixture",
                "license": "fixture",
                "license_url": "https://example.test/license",
            }

        bundle = {
            "assembly": {
                "dictionary_sha256": "b" * 64,
                "compatible_dictionary_sha256": [],
            },
            "assets": [entry(role) for role in build_bundles.REQUIRED_RUNTIME_ROLES],
        }
        build_bundles.validate_runtime_bundle(bundle)
        bundle["assets"][0]["path"] = "../escape"
        with self.assertRaisesRegex(ValueError, "unsafe path"):
            build_bundles.validate_runtime_bundle(bundle)
        bundle["assets"][0]["path"] = "hg38/file"
        bundle["assets"][0]["license"] = ""
        with self.assertRaisesRegex(ValueError, "incomplete license"):
            build_bundles.validate_runtime_bundle(bundle)


if __name__ == "__main__":
    unittest.main()
