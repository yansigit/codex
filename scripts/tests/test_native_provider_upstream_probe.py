from __future__ import annotations

import sys
import subprocess
import tempfile
import unittest
from unittest.mock import patch
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
                "codex-rs/config/config.schema.json",
                "codex-rs/config/src/thread_config/proto/codex.thread_config.v1.rs",
                "codex-rs/config/src/thread_config.proto",
                "codex-rs/app-server/exports/bundle.gz",
            ]
        )
        self.assertEqual(
            source,
            [
                "codex-rs/core/src/client.rs",
                "codex-rs/config/src/thread_config.proto",
            ],
        )
        self.assertEqual(
            generated,
            [
                "codex-rs/app-server/schema/json/Account.json",
                "codex-rs/app-server/exports/precomputed/stable.zst",
                "codex-rs/config/config.schema.json",
                "codex-rs/config/src/thread_config/proto/codex.thread_config.v1.rs",
                "codex-rs/app-server/exports/bundle.gz",
            ],
        )
        self.assertIn("codex-rs/config/src/thread_config.proto", source)

    def test_source_file_named_with_generated_suffix_is_categorized_by_path(
        self,
    ) -> None:
        source, generated = probe.classify_conflicts(
            ["codex-rs/core/src/generated/foo.rs"]
        )
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

    def test_cleanup_failure_never_prunes_unrelated_worktrees(self):
        with tempfile.TemporaryDirectory() as temp:
            repo = Path(temp) / "repo"
            repo.mkdir()

            def git(*args):
                return subprocess.check_output(
                    ["git", "-C", str(repo), *args],
                    text=True,
                    stderr=subprocess.DEVNULL,
                ).strip()

            git("init", "-b", "main")
            git(
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "--allow-empty",
                "-m",
                "base",
            )
            unrelated = Path(temp) / "occupied"
            git("worktree", "add", "--detach", str(unrelated))
            # A missing but registered worktree must not be pruned by this probe.
            import shutil

            shutil.rmtree(unrelated)
            before = git("worktree", "list", "--porcelain")
            original = probe.command
            calls = []

            def command(root, args):
                calls.append(args)
                if args[:2] == ["worktree", "remove"]:
                    return subprocess.CompletedProcess(
                        args, 1, "", "fixture removal failure"
                    )
                return original(root, args)

            with patch.object(probe, "command", side_effect=command):
                result = probe.run_probe(
                    repo, str(repo), "main", Path(temp) / "artifacts"
                )
            self.assertEqual(result["worktree_cleanup"], "failed")
            self.assertFalse(any(args[:2] == ["worktree", "prune"] for args in calls))
            self.assertIn(str(unrelated), before)
            self.assertIn(str(unrelated), git("worktree", "list", "--porcelain"))
            self.assertIn("deployment", result["outstanding_gates"])

    def test_fetch_failure_still_writes_artifacts_and_leaves_checkout_clean(
        self,
    ) -> None:
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
            (base / "base.txt").write_text("baseline\n")
            git("add", ".")
            git("commit", "-m", "base")
            base_sha = git("rev-parse", "HEAD")

            result = probe.run_probe(base, str(base), "missing-ref", output)

            self.assertEqual(result["merge_status"], "fetch-failed")
            self.assertEqual(result["base_sha"], base_sha)
            self.assertTrue((output / "probe.log").is_file())
            self.assertTrue((output / "probe.json").is_file())
            self.assertTrue((output / "artifact-sha256.json").is_file())
            self.assertEqual(git("rev-parse", "HEAD"), base_sha)
            self.assertEqual(git("status", "--porcelain"), "")

    def test_clean_probe_needs_no_persistent_git_identity(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp) / "repo"
            output = Path(temp) / "artifacts"
            base.mkdir()

            def git(*args: str, check: bool = True) -> str:
                result = subprocess.run(
                    ["git", "-C", str(base), *args],
                    check=check,
                    capture_output=True,
                    text=True,
                )
                return result.stdout.strip()

            git("init", "-b", "main")
            git("config", "user.name", "Fixture")
            git("config", "user.email", "fixture@example.invalid")
            (base / "source.txt").write_text("baseline\n")
            (base / "scripts").mkdir()
            (base / "scripts/native_provider_checks.py").write_text(
                "print('focused checks passed')\n"
            )
            git("add", ".")
            git("commit", "-m", "base")
            git("branch", "upstream")
            git("checkout", "upstream")
            (base / "source.txt").write_text("upstream change\n")
            git("commit", "-am", "upstream")
            upstream_sha = git("rev-parse", "HEAD")
            git("checkout", "main")
            base_sha = git("rev-parse", "HEAD")
            git("config", "--unset", "user.name")
            git("config", "--unset", "user.email")

            result = probe.run_probe(base, str(base), "refs/heads/upstream", output)

            self.assertEqual(result["merge_status"], "clean")
            self.assertEqual(result["base_sha"], base_sha)
            self.assertEqual(result["upstream_sha"], upstream_sha)
            self.assertEqual(result["focused_checks"], "passed")
            self.assertTrue(result["merge_tree"])
            self.assertEqual(git("rev-parse", "HEAD"), base_sha)
            self.assertEqual(git("status", "--porcelain"), "")
            self.assertNotIn(str(output), git("worktree", "list", "--porcelain"))
            self.assertTrue((output / "artifact-sha256.json").is_file())


if __name__ == "__main__":
    unittest.main()
