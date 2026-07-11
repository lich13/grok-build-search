from __future__ import annotations

import hashlib
import os
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
LAUNCHER = ROOT / "plugins" / "grok-build-search" / "scripts" / "grok-build-search-mcp"


class LauncherTests(unittest.TestCase):
    def setUp(self) -> None:
        self.assertTrue(LAUNCHER.is_file(), "release launcher must exist")

    def make_environment(
        self,
        directory: Path,
        *,
        system: str = "Darwin",
        machine: str = "arm64",
        checksum_matches: bool = True,
    ) -> tuple[dict[str, str], Path, Path]:
        tools = directory / "tools"
        tools.mkdir()
        payload = directory / "payload"
        payload.write_text(
            "#!/bin/sh\nprintf 'payload:%s\\n' \"$*\"\n",
            encoding="utf-8",
        )
        payload.chmod(0o755)
        payload_hash = hashlib.sha256(payload.read_bytes()).hexdigest()
        if not checksum_matches:
            payload_hash = "0" * 64
        curl_log = directory / "curl.log"

        uname = tools / "uname"
        uname.write_text(
            "#!/bin/sh\n"
            "if [ \"$1\" = \"-s\" ]; then printf '%s\\n' \"$FAKE_UNAME_S\"; "
            "else printf '%s\\n' \"$FAKE_UNAME_M\"; fi\n",
            encoding="utf-8",
        )
        uname.chmod(0o755)

        curl = tools / "curl"
        curl.write_text(
            "#!/bin/sh\n"
            "output=''\nurl=''\n"
            "while [ \"$#\" -gt 0 ]; do\n"
            "  case \"$1\" in\n"
            "    -o) shift; output=$1 ;;\n"
            "    http://*|https://*) url=$1 ;;\n"
            "  esac\n"
            "  shift\n"
            "done\n"
            "printf '%s\\n' \"$url\" >> \"$FAKE_CURL_LOG\"\n"
            "case \"$url\" in\n"
            "  */SHA256SUMS) printf '%s  %s\\n' \"$FAKE_SHA256\" \"$FAKE_ASSET\" > \"$output\" ;;\n"
            "  *) cp \"$FAKE_PAYLOAD\" \"$output\" ;;\n"
            "esac\n",
            encoding="utf-8",
        )
        curl.chmod(0o755)

        environment = os.environ.copy()
        environment.update(
            {
                "PATH": f"{tools}{os.pathsep}{environment['PATH']}",
                "FAKE_UNAME_S": system,
                "FAKE_UNAME_M": machine,
                "FAKE_PAYLOAD": str(payload),
                "FAKE_SHA256": payload_hash,
                "FAKE_CURL_LOG": str(curl_log),
                "GROK_BUILD_SEARCH_CACHE_DIR": str(directory / "cache"),
            }
        )
        return environment, curl_log, payload

    def run_launcher(
        self,
        environment: dict[str, str],
        *arguments: str,
    ) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [str(LAUNCHER), *arguments],
            check=False,
            capture_output=True,
            text=True,
            env=environment,
        )

    def test_explicit_binary_override_executes_without_download(self) -> None:
        with tempfile.TemporaryDirectory() as directory_text:
            directory = Path(directory_text)
            environment, curl_log, payload = self.make_environment(directory)
            environment["GROK_BUILD_SEARCH_MCP_BIN"] = str(payload)

            result = self.run_launcher(environment, "--version")

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout, "payload:--version\n")
            self.assertFalse(curl_log.exists())

    def test_first_run_downloads_and_second_run_reuses_verified_cache(self) -> None:
        with tempfile.TemporaryDirectory() as directory_text:
            directory = Path(directory_text)
            environment, curl_log, _payload = self.make_environment(directory)
            environment["FAKE_ASSET"] = "grok-build-search-mcp-darwin-aarch64"

            first = self.run_launcher(environment, "doctor")
            first_urls = curl_log.read_text(encoding="utf-8").splitlines()
            curl_log.unlink()
            second = self.run_launcher(environment, "doctor")

            self.assertEqual(first.returncode, 0, first.stderr)
            self.assertEqual(second.returncode, 0, second.stderr)
            self.assertEqual(first.stdout, "payload:doctor\n")
            self.assertEqual(len(first_urls), 2)
            self.assertTrue(all("/v0.1.1/" in url for url in first_urls))
            self.assertTrue(first_urls[0].endswith("/SHA256SUMS"))
            self.assertTrue(first_urls[1].endswith("/grok-build-search-mcp-darwin-aarch64"))
            self.assertFalse(curl_log.exists(), "cached launch must not call curl")

    def test_tampered_cached_binary_is_replaced(self) -> None:
        with tempfile.TemporaryDirectory() as directory_text:
            directory = Path(directory_text)
            environment, curl_log, _payload = self.make_environment(directory)
            asset = "grok-build-search-mcp-darwin-aarch64"
            environment["FAKE_ASSET"] = asset
            self.assertEqual(self.run_launcher(environment).returncode, 0)
            cached = directory / "cache" / "v0.1.1" / asset
            cached.write_text("tampered", encoding="utf-8")
            curl_log.unlink()

            result = self.run_launcher(environment, "again")

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout, "payload:again\n")
            self.assertTrue(curl_log.exists(), "tampered cache must be downloaded again")

    def test_checksum_mismatch_refuses_to_install(self) -> None:
        with tempfile.TemporaryDirectory() as directory_text:
            directory = Path(directory_text)
            environment, _curl_log, _payload = self.make_environment(
                directory,
                checksum_matches=False,
            )
            asset = "grok-build-search-mcp-darwin-aarch64"
            environment["FAKE_ASSET"] = asset

            result = self.run_launcher(environment)

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("SHA-256", result.stderr)
            self.assertFalse((directory / "cache" / "v0.1.1" / asset).exists())

    def test_maps_all_supported_release_assets(self) -> None:
        cases = [
            ("Darwin", "arm64", "grok-build-search-mcp-darwin-aarch64"),
            ("Darwin", "x86_64", "grok-build-search-mcp-darwin-x86_64"),
            ("Linux", "aarch64", "grok-build-search-mcp-linux-aarch64"),
            ("Linux", "x86_64", "grok-build-search-mcp-linux-x86_64"),
        ]
        for system, machine, asset in cases:
            with self.subTest(system=system, machine=machine):
                with tempfile.TemporaryDirectory() as directory_text:
                    directory = Path(directory_text)
                    environment, curl_log, _payload = self.make_environment(
                        directory,
                        system=system,
                        machine=machine,
                    )
                    environment["FAKE_ASSET"] = asset

                    result = self.run_launcher(environment)

                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertTrue(
                        curl_log.read_text(encoding="utf-8").splitlines()[-1].endswith(asset)
                    )

    def test_rejects_unsupported_platform_before_download(self) -> None:
        with tempfile.TemporaryDirectory() as directory_text:
            directory = Path(directory_text)
            environment, curl_log, _payload = self.make_environment(
                directory,
                system="Windows_NT",
                machine="x86_64",
            )
            environment["FAKE_ASSET"] = "unused"

            result = self.run_launcher(environment)

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("Unsupported platform", result.stderr)
            self.assertFalse(curl_log.exists())


if __name__ == "__main__":
    unittest.main()
