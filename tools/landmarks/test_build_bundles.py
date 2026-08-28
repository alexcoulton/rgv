import gzip
import tempfile
import unittest
from pathlib import Path

import build_bundles


class LandmarkBundleTests(unittest.TestCase):
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


if __name__ == "__main__":
    unittest.main()
