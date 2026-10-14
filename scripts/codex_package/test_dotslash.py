import hashlib
import io
import json
import socket
import ssl
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import call, patch
from urllib.error import HTTPError, URLError

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from codex_package import dotslash
from codex_package.targets import TARGET_SPECS


class DownloadArchiveTest(unittest.TestCase):
    url = "https://example.test/artifact.tar.gz"

    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.archive = self.root / "artifact.tar.gz"
        self.partial = self.root / "artifact.tar.gz.tmp"

    def test_retries_dns_failure_then_installs_complete_archive(self) -> None:
        error = URLError(socket.gaierror(socket.EAI_NONAME, "DNS unavailable"))
        with (
            patch.object(
                dotslash, "urlopen", side_effect=[error, io.BytesIO(b"archive")]
            ) as download,
            patch.object(dotslash.time, "sleep") as sleep,
        ):
            dotslash.download_archive(self.url, self.archive)

        self.assertEqual(self.archive.read_bytes(), b"archive")
        self.assertFalse(self.partial.exists())
        self.assertEqual(download.call_count, 2)
        self.assertEqual(
            download.call_args.kwargs, {"timeout": dotslash.DOWNLOAD_TIMEOUT_SECS}
        )
        sleep.assert_called_once_with(2)

    def test_exhausted_timeouts_preserve_existing_archive_and_remove_partial(
        self,
    ) -> None:
        self.archive.write_bytes(b"previous archive")
        self.partial.write_bytes(b"stale partial")
        with (
            patch.object(
                dotslash, "urlopen", side_effect=URLError(TimeoutError())
            ) as download,
            patch.object(dotslash.time, "sleep") as sleep,
            self.assertRaises(URLError),
        ):
            dotslash.download_archive(self.url, self.archive)

        self.assertEqual(download.call_count, 3)
        self.assertEqual(sleep.call_args_list, [call(2), call(4)])
        self.assertEqual(self.archive.read_bytes(), b"previous archive")
        self.assertFalse(self.partial.exists())

    def test_disconnected_response_restarts_without_appending_partial_bytes(
        self,
    ) -> None:
        class InterruptedResponse(io.BytesIO):
            def read(self, size: int = -1) -> bytes:
                if self.tell():
                    raise ConnectionResetError("connection reset")
                return super().read(size)

        with (
            patch.object(
                dotslash,
                "urlopen",
                side_effect=[InterruptedResponse(b"partial"), io.BytesIO(b"complete")],
            ) as download,
            patch.object(dotslash.time, "sleep"),
        ):
            dotslash.download_archive(self.url, self.archive)

        self.assertEqual(download.call_count, 2)
        self.assertEqual(self.archive.read_bytes(), b"complete")
        self.assertFalse(self.partial.exists())

    def test_http_and_certificate_failures_are_not_retried(self) -> None:
        errors = [
            HTTPError(self.url, code, "rejected", {}, None) for code in (401, 403, 404)
        ] + [URLError(ssl.SSLCertVerificationError("untrusted certificate"))]
        for error in errors:
            with (
                self.subTest(error=error),
                patch.object(dotslash, "urlopen", side_effect=error) as download,
                patch.object(dotslash.time, "sleep") as sleep,
            ):
                with self.assertRaises(URLError):
                    dotslash.download_archive(self.url, self.archive)
                download.assert_called_once()
                sleep.assert_not_called()
                self.assertFalse(self.archive.exists())
                self.assertFalse(self.partial.exists())

    def test_local_write_failure_is_not_retried(self) -> None:
        with (
            patch.object(
                dotslash, "urlopen", return_value=io.BytesIO(b"archive")
            ) as download,
            patch("builtins.open", side_effect=PermissionError("read-only directory")),
            patch.object(dotslash.time, "sleep") as sleep,
            self.assertRaises(PermissionError),
        ):
            dotslash.download_archive(self.url, self.archive)
        download.assert_called_once()
        sleep.assert_not_called()
        self.assertFalse(self.partial.exists())

    def test_retry_does_not_bypass_pinned_archive_digest(self) -> None:
        manifest = self.root / "manifest.json"
        spec = TARGET_SPECS["x86_64-apple-darwin"]
        manifest.write_text(
            json.dumps(
                {
                    "platforms": {
                        spec.dotslash_platform: {
                            "size": 4,
                            "hash": "sha256",
                            "digest": hashlib.sha256(b"good").hexdigest(),
                            "format": "tar.gz",
                            "path": "rg",
                            "providers": [{"url": self.url}],
                        }
                    }
                }
            )
        )
        with (
            patch.object(dotslash, "default_cache_root", return_value=self.root),
            patch.object(
                dotslash,
                "urlopen",
                side_effect=[URLError(TimeoutError()), io.BytesIO(b"evil")],
            ) as download,
            patch.object(dotslash.time, "sleep"),
            self.assertRaisesRegex(RuntimeError, "sha256"),
        ):
            dotslash.fetch_dotslash_executable(
                spec,
                manifest_path=manifest,
                artifact_label="test artifact",
                cache_key="cache",
                dest_name="rg",
            )
        self.assertEqual(download.call_count, 2)
        self.assertEqual(list((self.root / "cache").iterdir()), [])


if __name__ == "__main__":
    unittest.main()
