import json
import tempfile
import unittest
from pathlib import Path

import stage_resources


class StageResourceTests(unittest.TestCase):
    def test_compact_selection_excludes_only_detailed_transcripts(self):
        catalogue = {
            "bundles": [
                {
                    "assets": [
                        {"role": role, "path": f"hg38/{role}"}
                        for role in sorted(
                            stage_resources.COMPACT_ROLES | {"detailed_transcripts"}
                        )
                    ]
                }
            ]
        }
        compact = stage_resources.selected_assets(catalogue, False)
        complete = stage_resources.selected_assets(catalogue, True)
        self.assertEqual(len(compact), len(stage_resources.COMPACT_ROLES))
        self.assertEqual(len(complete), len(stage_resources.COMPACT_ROLES) + 1)
        self.assertNotIn("detailed_transcripts", {asset["role"] for asset in compact})

    def test_pending_distribution_review_is_an_explicit_staging_gate(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            bundle = root / "hg38"
            bundle.mkdir()
            (bundle / "manifest.json").write_text(
                json.dumps(
                    {
                        "distribution_status": (
                            "local_qualification_only_pending_release_review"
                        )
                    }
                ),
                encoding="utf-8",
            )
            catalogue = {"bundles": [{"id": "hg38"}]}
            with self.assertRaisesRegex(ValueError, "distribution review"):
                stage_resources.validate_release_gate(root, catalogue, False)
            stage_resources.validate_release_gate(root, catalogue, True)

    def test_cross_platform_unsafe_paths_are_rejected(self):
        for value in ("../escape", "C:/escape", "bundle\\escape"):
            with self.subTest(value=value), self.assertRaisesRegex(ValueError, "unsafe"):
                stage_resources.safe_path(value)


if __name__ == "__main__":
    unittest.main()
