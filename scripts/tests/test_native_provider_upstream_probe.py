from __future__ import annotations

import sys
import subprocess
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import native_provider_upstream_probe as probe


class NativeProviderUpstreamProbeTests(unittest.TestCase):
    def test_source_and_generated_conflicts_are_reported_separately(self) -> None:
        source, generated = probe.classify_conflicts(
            [
                "codex-rs/core/src/client.rs",
                "codex-rs/app-server/schema/json/Account.json",
                "codex-rs/app-server/exports/precomputed/stable.zst",
            ]
        )
        self.assertEqual(source, ["codex-rs/core/src/client.rs"])
        self.assertEqual(
            generated,
            [
                "codex-rs/app-server/schema/json/Account.json",
                "codex-rs/app-server/exports/precomputed/stable.zst",
            ],
        )

    def test_source_file_named_with_generated_suffix_is_categorized_by_path(self) -> None:
        source, generated = probe.classify_conflicts(["codex-rs/core/src/generated/foo.rs"])
        self.assertEqual(source, [])
        self.assertEqual(generated, ["codex-rs/core/src/generated/foo.rs"])

    def test_probe_records_separate_conflicts_without_changing_checkout(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp) / "repo"
            output = Path(temp) / "artifacts"
            base.mkdir()

            def git(*args: str) -> str:
                result = subprocess.run(
                    ["git", "-C", str(base), *args],
                    check=True,
                    capture_output=True,
                    text=True,
                )
                return result.stdout.strip()

            git("init", "-b", "main")
            git("config", "user.name", "Fixture")
            git("config", "user.email", "fixture@example.invalid")
            (base / "core.rs").write_text("base source\n")
            (base / "schema/generated.json").parent.mkdir(parents=True)
            (base / "schema/generated.json").write_text("base schema\n")
            git("add", ".")
            git("commit", "-m", "base")
            git("branch", "upstream")
            git("checkout", "upstream")
            (base / "core.rs").write_text("upstream source\n")
            (base / "schema/generated.json").write_text("upstream schema\n")
            git("commit", "-am", "upstream changes")
            upstream_sha = git("rev-parse", "HEAD")
            git("checkout", "main")
            (base / "core.rs").write_text("local source\n")
            (base / "schema/generated.json").write_text("local schema\n")
            git("commit", "-am", "local changes")
            base_sha = git("rev-parse", "HEAD")

            result = probe.run_probe(base, str(base), "refs/heads/upstream", output)

            self.assertEqual(result["merge_status"], "conflicts")
            self.assertEqual(result["base_sha"], base_sha)
            self.assertEqual(result["upstream_sha"], upstream_sha)
            self.assertEqual(result["source_conflicts"], ["core.rs"])
            self.assertEqual(result["generated_conflicts"], ["schema/generated.json"])
            self.assertFalse(result["push_attempted"])
            self.assertEqual(git("rev-parse", "HEAD"), base_sha)
            self.assertEqual(git("status", "--porcelain"), "")
            self.assertTrue((output / "artifact-sha256.json").is_file())


if __name__ == "__main__":
    unittest.main()
