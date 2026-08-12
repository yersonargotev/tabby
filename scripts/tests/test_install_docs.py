#!/usr/bin/env python3

from __future__ import annotations

import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]


class InstallDocumentationTests(unittest.TestCase):
    def test_marketplace_install_documents_the_complete_trust_surface(self) -> None:
        readme = (REPO_ROOT / "README.md").read_text()
        install_guide = (REPO_ROOT / "docs" / "install.md").read_text()
        documentation = f"{readme}\n{install_guide}".lower()

        for required_text in (
            "herdr.dev/plugins",
            "herdr-plugin",
            "herdr plugin install yersonargotev/tabby",
            "not a security review or endorsement",
            "unsandboxed",
            "sha-256",
            "herdr plugin config-dir yersonargotev.tabby",
            "herdr plugin action invoke start --plugin yersonargotev.tabby",
            "homebrew remains an optional alternative",
            "docs/evidence/issue-71-herdr-0.8-lifecycle.md",
            "docs/evidence/issue-79-herdr-native-release.md",
        ):
            self.assertIn(required_text, documentation)


if __name__ == "__main__":
    unittest.main()
