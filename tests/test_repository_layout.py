from __future__ import annotations

import json
import tomllib
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


class RepositoryLayoutTests(unittest.TestCase):
    def test_ci_and_release_workflows_cover_supported_targets(self) -> None:
        ci_path = ROOT / ".github" / "workflows" / "ci.yml"
        release_path = ROOT / ".github" / "workflows" / "release.yml"
        self.assertTrue(ci_path.is_file(), "CI workflow must exist")
        self.assertTrue(release_path.is_file(), "release workflow must exist")

        ci = ci_path.read_text(encoding="utf-8")
        for required in [
            "uses: actions/checkout@v7",
            "uses: actions/setup-python@v6",
            'python-version: "3.12"',
            "cargo fmt --all -- --check",
            "cargo clippy --all-targets --locked -- -D warnings",
            "cargo test --all-targets --locked",
            "python3 -m unittest discover -s tests -p 'test_*.py'",
        ]:
            self.assertIn(required, ci)

        release = release_path.read_text(encoding="utf-8")
        for required in [
            "uses: actions/checkout@v7",
            "uses: actions/upload-artifact@v7",
            "uses: actions/download-artifact@v8",
        ]:
            self.assertIn(required, release)
        targets = {
            "aarch64-apple-darwin": "grok-build-search-mcp-darwin-aarch64",
            "x86_64-apple-darwin": "grok-build-search-mcp-darwin-x86_64",
            "aarch64-unknown-linux-gnu": "grok-build-search-mcp-linux-aarch64",
            "x86_64-unknown-linux-gnu": "grok-build-search-mcp-linux-x86_64",
        }
        for target, asset in targets.items():
            self.assertIn(target, release)
            self.assertIn(asset, release)
        self.assertIn("tags:", release)
        self.assertIn("SHA256SUMS", release)
        self.assertIn("gh release create", release)

    def test_readme_documents_public_install_and_runtime_contract(self) -> None:
        readme = (ROOT / "README.md").read_text(encoding="utf-8")
        normalized = " ".join(readme.split())
        for required in [
            "codex plugin marketplace add https://github.com/lich13/grok-build-search.git",
            "codex plugin add grok-build-search@grok-build-search",
            "Grok Build CLI `>=0.2.93,<0.3.0`",
            "web_search",
            "web_fetch",
            "doctor",
            "SHA-256",
            "not affiliated with or endorsed by xAI or OpenAI",
        ]:
            self.assertIn(required, normalized)

    def test_rust_package_metadata_is_pinned(self) -> None:
        cargo_toml = ROOT / "Cargo.toml"
        self.assertTrue(cargo_toml.is_file(), "Cargo.toml must exist")

        metadata = tomllib.loads(cargo_toml.read_text(encoding="utf-8"))
        package = metadata["package"]
        self.assertEqual(package["name"], "grok-build-search-mcp")
        self.assertEqual(package["version"], "0.1.0")
        self.assertEqual(package["edition"], "2024")
        self.assertEqual(package["rust-version"], "1.94.1")

        toolchain = tomllib.loads(
            (ROOT / "rust-toolchain.toml").read_text(encoding="utf-8")
        )
        self.assertEqual(toolchain["toolchain"]["channel"], "1.94.1")

    def test_plugin_manifest_exposes_skill_and_mcp_server(self) -> None:
        plugin_root = ROOT / "plugins" / "grok-build-search"
        manifest_path = plugin_root / ".codex-plugin" / "plugin.json"
        self.assertTrue(manifest_path.is_file(), "plugin.json must exist")
        manifest = json.loads(
            manifest_path.read_text(encoding="utf-8")
        )

        self.assertEqual(manifest["name"], "grok-build-search")
        self.assertEqual(manifest["version"], "0.1.0")
        self.assertEqual(manifest["author"]["name"], "lich13")
        self.assertEqual(manifest["license"], "MIT")
        self.assertEqual(manifest["skills"], "./skills/")
        self.assertEqual(manifest["mcpServers"], "./.mcp.json")
        self.assertNotIn("apps", manifest)
        self.assertNotIn("hooks", manifest)

        mcp_path = plugin_root / ".mcp.json"
        self.assertTrue(mcp_path.is_file(), ".mcp.json must exist")
        mcp = json.loads(mcp_path.read_text(encoding="utf-8"))
        server = mcp["mcpServers"]["grok-build-search"]
        self.assertEqual(server["command"], "./scripts/grok-build-search-mcp")
        self.assertEqual(server["args"], [])
        self.assertEqual(server["cwd"], ".")
        self.assertEqual(server["env_vars"], [
            "GROK_BUILD_SEARCH_MCP_BIN",
            "GROK_BUILD_SEARCH_CACHE_DIR",
            "GROK_BIN",
        ])

    def test_repository_marketplace_points_to_plugin(self) -> None:
        marketplace_path = ROOT / ".agents" / "plugins" / "marketplace.json"
        self.assertTrue(marketplace_path.is_file(), "marketplace.json must exist")
        marketplace = json.loads(
            marketplace_path.read_text(encoding="utf-8")
        )

        self.assertEqual(marketplace["name"], "grok-build-search")
        self.assertEqual(marketplace["interface"]["displayName"], "Grok Build Search")
        self.assertEqual(len(marketplace["plugins"]), 1)

        entry = marketplace["plugins"][0]
        self.assertEqual(entry["name"], "grok-build-search")
        self.assertEqual(entry["source"], {
            "source": "local",
            "path": "./plugins/grok-build-search",
        })
        self.assertEqual(entry["policy"], {
            "installation": "AVAILABLE",
            "authentication": "ON_INSTALL",
        })
        self.assertEqual(entry["category"], "Productivity")

    def test_grok_search_skill_declares_routing_and_fallback_policy(self) -> None:
        skill_root = (
            ROOT
            / "plugins"
            / "grok-build-search"
            / "skills"
            / "grok-search"
        )
        skill_path = skill_root / "SKILL.md"
        agent_path = skill_root / "agents" / "openai.yaml"
        self.assertTrue(skill_path.is_file(), "grok-search SKILL.md must exist")
        self.assertTrue(agent_path.is_file(), "grok-search openai.yaml must exist")

        skill = skill_path.read_text(encoding="utf-8")
        self.assertIn("name: grok-search", skill)
        self.assertIn("description: Use when", skill)
        for required in [
            "web_search",
            "web_fetch",
            "doctor",
            "NO_SOURCES",
            "INVALID_URL",
            "PRIVATE_URL",
            "Codex",
            "fallback is unavailable",
        ]:
            self.assertIn(required, skill)

        agent = agent_path.read_text(encoding="utf-8")
        self.assertIn('$grok-search', agent)
        self.assertIn('dependencies:', agent)
        self.assertIn('type: "mcp"', agent)
        self.assertIn('value: "grok-build-search"', agent)


if __name__ == "__main__":
    unittest.main()
