from __future__ import annotations

import json
import os
import subprocess
import tempfile
import time
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
FAKE_GROK = ROOT / "tests" / "fixtures" / "fake-grok"


class FakeGrokTests(unittest.TestCase):
    def run_fake(self, mode: str) -> subprocess.CompletedProcess[str]:
        with tempfile.TemporaryDirectory() as directory:
            prompt_file = Path(directory) / "prompt.txt"
            prompt_file.write_text("test prompt", encoding="utf-8")
            environment = os.environ.copy()
            environment["FAKE_GROK_MODE"] = mode
            return subprocess.run(
                [str(FAKE_GROK), "--prompt-file", str(prompt_file)],
                check=False,
                capture_output=True,
                text=True,
                env=environment,
            )

    def test_search_success_matches_real_json_shape(self) -> None:
        self.assertTrue(FAKE_GROK.is_file(), "fake-grok fixture must exist")
        with tempfile.TemporaryDirectory() as directory:
            prompt_file = Path(directory) / "prompt.txt"
            log_file = Path(directory) / "invocation.json"
            prompt_file.write_text("find current Rust release", encoding="utf-8")

            environment = os.environ.copy()
            environment["FAKE_GROK_MODE"] = "search-success"
            environment["FAKE_GROK_LOG"] = str(log_file)
            result = subprocess.run(
                [
                    str(FAKE_GROK),
                    "--output-format",
                    "json",
                    "--prompt-file",
                    str(prompt_file),
                ],
                check=True,
                capture_output=True,
                text=True,
                env=environment,
            )

            payload = json.loads(result.stdout)
            self.assertEqual(payload["stopReason"], "end_turn")
            self.assertEqual(payload["sessionId"], "fake-session-id")
            self.assertEqual(payload["requestId"], "fake-request-id")
            self.assertIn("https://www.rust-lang.org/", payload["text"])
            self.assertIn("thought", payload)

            invocation = json.loads(log_file.read_text(encoding="utf-8"))
            self.assertEqual(invocation["prompt"], "find current Rust release")
            self.assertEqual(invocation["mode"], "search-success")

    def test_log_records_isolation_metadata(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            workdir = Path(directory)
            prompt_file = workdir / "prompt.txt"
            log_file = workdir / "invocation.json"
            prompt_file.write_text("isolated prompt", encoding="utf-8")
            prompt_file.chmod(0o600)
            environment = os.environ.copy()
            environment["FAKE_GROK_LOG"] = str(log_file)
            environment["GROK_WEB_FETCH"] = "1"

            subprocess.run(
                [str(FAKE_GROK), "--prompt-file", str(prompt_file)],
                check=True,
                capture_output=True,
                text=True,
                env=environment,
                cwd=workdir,
            )

            invocation = json.loads(log_file.read_text(encoding="utf-8"))
            self.assertEqual(invocation.get("grok_web_fetch"), "1")
            self.assertEqual(invocation.get("prompt_mode"), "0600")
            self.assertEqual(Path(invocation.get("cwd", "")).resolve(), workdir.resolve())

    def test_log_records_api_key_environment_for_runner_audit(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            prompt_file = Path(directory) / "prompt.txt"
            log_file = Path(directory) / "invocation.json"
            prompt_file.write_text("audit environment", encoding="utf-8")
            environment = os.environ.copy()
            environment["FAKE_GROK_LOG"] = str(log_file)
            environment["XAI_API_KEY"] = "xai-secret"
            environment["OPENAI_API_KEY"] = "openai-secret"
            environment["ANTHROPIC_API_KEY"] = "anthropic-secret"

            subprocess.run(
                [str(FAKE_GROK), "--prompt-file", str(prompt_file)],
                check=True,
                capture_output=True,
                text=True,
                env=environment,
            )

            invocation = json.loads(log_file.read_text(encoding="utf-8"))
            self.assertEqual(invocation.get("xai_api_key"), "xai-secret")
            self.assertEqual(invocation.get("openai_api_key"), "openai-secret")
            self.assertEqual(invocation.get("anthropic_api_key"), "anthropic-secret")

    def test_version_matches_supported_grok_release(self) -> None:
        self.assertTrue(FAKE_GROK.is_file(), "fake-grok fixture must exist")
        result = subprocess.run(
            [str(FAKE_GROK), "--version"],
            check=True,
            capture_output=True,
            text=True,
        )

        self.assertEqual(result.stdout.strip(), "grok 0.2.93 (f00f96316d4b)")

    def test_version_can_simulate_unsupported_release(self) -> None:
        environment = os.environ.copy()
        environment["FAKE_GROK_VERSION"] = "grok 0.3.0 (future)"
        result = subprocess.run(
            [str(FAKE_GROK), "--version"],
            check=True,
            capture_output=True,
            text=True,
            env=environment,
        )

        self.assertEqual(result.stdout.strip(), "grok 0.3.0 (future)")

    def test_bad_json_mode_succeeds_with_invalid_stdout(self) -> None:
        result = self.run_fake("bad-json")

        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout.strip(), "not-json")

    def test_exit_failed_mode_returns_nonzero_and_stderr(self) -> None:
        result = self.run_fake("exit-failed")

        self.assertEqual(result.returncode, 17)
        self.assertIn("simulated Grok failure", result.stderr)

    def test_exit_failed_detail_can_include_prompt_path_and_secret(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            prompt_file = Path(directory) / "prompt.txt"
            prompt_file.write_text("test prompt", encoding="utf-8")
            environment = os.environ.copy()
            environment["FAKE_GROK_MODE"] = "exit-failed"
            environment["FAKE_GROK_ERROR_DETAIL"] = "1"
            result = subprocess.run(
                [str(FAKE_GROK), "--prompt-file", str(prompt_file)],
                check=False,
                capture_output=True,
                text=True,
                env=environment,
            )

        self.assertEqual(result.returncode, 17)
        self.assertIn(str(prompt_file), result.stderr)
        self.assertIn("token=secret-value", result.stderr)

    def test_no_sources_mode_returns_valid_uncited_json(self) -> None:
        result = self.run_fake("no-sources")

        self.assertEqual(result.returncode, 0)
        self.assertEqual(json.loads(result.stdout)["text"], "Uncited answer.")

    def test_stderr_warning_mode_still_succeeds(self) -> None:
        result = self.run_fake("stderr-warning")

        self.assertEqual(result.returncode, 0)
        self.assertIn("simulated configuration warning", result.stderr)
        self.assertIn("https://example.com/warning", result.stdout)

    def test_sleep_mode_waits_for_configured_duration(self) -> None:
        environment = os.environ.copy()
        environment["FAKE_GROK_MODE"] = "sleep"
        environment["FAKE_GROK_SLEEP_SECONDS"] = "0.05"
        with tempfile.TemporaryDirectory() as directory:
            prompt_file = Path(directory) / "prompt.txt"
            prompt_file.write_text("test prompt", encoding="utf-8")
            started = time.monotonic()
            result = subprocess.run(
                [str(FAKE_GROK), "--prompt-file", str(prompt_file)],
                check=False,
                capture_output=True,
                text=True,
                env=environment,
            )

        self.assertEqual(result.returncode, 0)
        self.assertGreaterEqual(time.monotonic() - started, 0.04)
        self.assertIn("https://example.com/sleep", result.stdout)


if __name__ == "__main__":
    unittest.main()
