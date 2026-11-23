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

    def test_hg19_qualifies_the_1000_genomes_b37_decoy_dictionary(self):
        self.assertIn(
            "d74373ee19d2e0f8378f17ea869b31f242189d381b2e04be48f6fbd76fb7028b",
            build_bundles.BUILD_SPECS["hg19"].compatible_dictionary_sha256,
        )

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

    def test_full_gene_list_prefers_mane_select_and_keeps_every_symbol(self):
        rows = [
            # ACTB: the longer NM_2 would win alone; MANE Select picks NM_1.2.
            "0\tNM_1.2\tchr1\t+\t0\t60\t10\t50\t1\t0,\t60,\t0\tACTB\tcmpl\tcmpl\t0,\n",
            "0\tNM_2.1\tchr1\t+\t0\t90\t10\t80\t1\t0,\t90,\t0\tACTB\tcmpl\tcmpl\t0,\n",
            # Renamed since MANE 1.0: found by both names.
            "0\tNM_3.1\tchr1\t-\t100\t150\t100\t150\t1\t100,\t150,\t0\tBMAL1\tcmpl\tcmpl\t0,\n",
            # No MANE record matches this accession; same symbol, other version.
            "0\tNM_5.4\tchr1\t+\t160\t170\t160\t170\t1\t160,\t170,\t0\tCCNA1\tcmpl\tcmpl\t0,\n",
            # Only in the full RefSeq list (like PBRM1): searchable by its symbol.
            "0\tNM_4.1\tchr2\t-\t10\t90\t20\t80\t1\t10,\t90,\t0\tPBRM1\tcmpl\tcmpl\t0,\n",
        ]
        preferred = build_bundles.PreferredTranscripts.from_records(
            [
                build_bundles.PreferredTranscript("NM_1.2", "chr1", "ACTB", "ENSG1.1", "ENST1.1"),
                build_bundles.PreferredTranscript("NM_3.1", "chr1", "ARNTL", "ENSG3.1", "ENST3.1"),
                build_bundles.PreferredTranscript("NM_9.1", "chr1", "CCNA1", "ENSG5.1", "ENST9.1"),
            ]
        )
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "source.gz"
            with gzip.open(source, "wt", encoding="utf-8") as stream:
                stream.writelines(rows)
            genes = root / "genes.gz"
            names = root / "names.gz"
            counts = build_bundles.curate_refgene(
                source,
                genes,
                names,
                {"chr1": 200, "chr2": 100},
                ["chr1", "chr2"],
                preferred=preferred,
            )
            self.assertEqual(build_bundles.validate_curated_gene_assets(genes, names), 4)
            with gzip.open(names, "rt", encoding="utf-8") as stream:
                stream.readline()
                search = {
                    fields[1]: fields
                    for fields in (line.rstrip("\n").split("\t") for line in stream)
                }
        self.assertEqual(sorted(search), ["ACTB", "BMAL1", "CCNA1", "PBRM1"])
        self.assertEqual(search["ACTB"][2:4], ["ENSG1.1", "NM_1.2"])
        self.assertEqual(search["ACTB"][8], "ENST1.1")
        self.assertEqual(search["BMAL1"][2], "ENSG3.1")
        self.assertEqual(search["BMAL1"][8], "ARNTL,ENST3.1")
        # Matched by symbol only: its Ensembl gene and transcript still find it.
        self.assertEqual(search["CCNA1"][2], "ENSG5.1")
        self.assertEqual(search["CCNA1"][8], "ENST9.1")
        self.assertEqual(
            search["PBRM1"], ["pbrm1", "PBRM1", "PBRM1", "NM_4.1", "chr2", "10", "90", "-", ""]
        )
        self.assertEqual(counts["genes"], 4)
        self.assertEqual(counts["preferred_representatives"], 2)
        self.assertEqual(counts["preferred_gene_ids"], 3)

    def test_offline_mode_never_downloads_a_missing_source(self):
        with tempfile.TemporaryDirectory() as temporary:
            destination = Path(temporary) / "missing.gz"
            build_bundles.NETWORK_ALLOWED = False
            try:
                with self.assertRaisesRegex(ValueError, "--offline forbids downloading"):
                    build_bundles.download("https://example.invalid/missing.gz", destination)
                existing = Path(temporary) / "present.gz"
                existing.write_bytes(b"kept")
                build_bundles.download("https://example.invalid/present.gz", existing)
                self.assertEqual(existing.read_bytes(), b"kept")
            finally:
                build_bundles.NETWORK_ALLOWED = True

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
