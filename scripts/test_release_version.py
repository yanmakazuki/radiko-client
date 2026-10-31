"""Offline checks for the v* release tag rule. No GitHub API and no network."""

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from release_version import (
    ReleaseCheckError,
    asset_basename,
    cargo_package_version,
    changelog_section,
    parse_tag,
    shared_cargo_version,
    write_env,
)

ROOT = Path(__file__).resolve().parents[1]

RADIKO = """\
[package]
name = "radiko"
version = "0.1.0"

[dependencies]
serde = { version = "1.0", features = ["derive"] }
"""

CLIENT = """\
[package]
name = "radiko-client"
version = "0.1.0"
"""

CHANGELOG = """\
# Changelog

## [Unreleased]

## [0.1.0] - 2026-10-31

### Added

- macOS Apple silicon archive

## [0.0.1] - 2026-01-01

### Added

- older

[Unreleased]: https://example.test/compare/v0.1.0...HEAD
[0.1.0]: https://example.test/releases/tag/v0.1.0
"""


class CargoVersionTests(unittest.TestCase):
    def test_package_version_ignores_dependency_versions(self):
        self.assertEqual(cargo_package_version(RADIKO, "radiko"), "0.1.0")

    def test_both_crates_must_agree(self):
        other = CLIENT.replace('version = "0.1.0"', 'version = "0.2.0"', 1)
        with self.assertRaises(ReleaseCheckError) as raised:
            shared_cargo_version(RADIKO, other)
        self.assertIn("No release is created", str(raised.exception))

    def test_suffix_in_cargo_toml_is_rejected(self):
        text = CLIENT.replace('version = "0.1.0"', 'version = "0.1.0-test.1"', 1)
        with self.assertRaises(ReleaseCheckError) as raised:
            cargo_package_version(text, "client")
        self.assertIn("No release is created", str(raised.exception))


class TagTests(unittest.TestCase):
    def test_exact_tag_is_a_stable_release(self):
        decision = parse_tag("v0.1.0", "0.1.0")
        self.assertEqual(decision.cargo_version, "0.1.0")
        self.assertEqual(decision.tag_version, "0.1.0")
        self.assertFalse(decision.prerelease)
        self.assertEqual(
            asset_basename(decision.tag_version),
            "radiko-client-0.1.0-aarch64-apple-darwin",
        )

    def test_semver_suffix_is_a_prerelease_and_cargo_version_stays_plain(self):
        decision = parse_tag("v0.1.0-test.1", "0.1.0")
        self.assertEqual(decision.cargo_version, "0.1.0")
        self.assertEqual(decision.tag_version, "0.1.0-test.1")
        self.assertTrue(decision.prerelease)
        self.assertEqual(
            asset_basename(decision.tag_version),
            "radiko-client-0.1.0-test.1-aarch64-apple-darwin",
        )
        self.assertIn("RELEASE_VERSION=0.1.0\n", decision.env_text())
        self.assertIn("PRERELEASE=true\n", decision.env_text())

    def test_rc_suffix_is_also_a_prerelease(self):
        decision = parse_tag("v0.1.0-rc.1", "0.1.0")
        self.assertTrue(decision.prerelease)
        self.assertEqual(decision.tag_version, "0.1.0-rc.1")

    def test_different_core_refuses_before_any_release(self):
        with self.assertRaises(ReleaseCheckError) as raised:
            parse_tag("v0.2.0", "0.1.0")
        self.assertIn("does not match Cargo.toml version 0.1.0", str(raised.exception))
        self.assertIn("No release is created", str(raised.exception))

    def test_suffix_does_not_bypass_a_different_core(self):
        with self.assertRaises(ReleaseCheckError) as raised:
            parse_tag("v0.2.0-test.1", "0.1.0")
        self.assertIn("No release is created", str(raised.exception))

    def test_incomplete_version_build_metadata_and_empty_suffix_fail(self):
        for tag in ("v0.1", "v0.1.0+gha", "v0.1.0-", "v0.1.0-01", "vv0.1.0", "0.1.0"):
            with self.assertRaises(ReleaseCheckError):
                parse_tag(tag, "0.1.0")

    def test_failed_check_does_not_write_an_env_file(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "release.env"
            with self.assertRaises(ReleaseCheckError):
                decision = parse_tag("v0.2.0", "0.1.0")
                write_env(decision, path)
            self.assertFalse(path.exists())

    def test_successful_check_writes_the_asset_name(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "release.env"
            write_env(parse_tag("v0.1.0-test.1", "0.1.0"), path)
            text = path.read_text()
            self.assertIn(
                "ASSET_BASENAME=radiko-client-0.1.0-test.1-aarch64-apple-darwin\n",
                text,
            )


class ChangelogTests(unittest.TestCase):
    def test_section_is_the_matching_version_only(self):
        body = changelog_section(CHANGELOG, "0.1.0")
        self.assertTrue(body.startswith("## [0.1.0] - 2026-10-31\n"))
        self.assertIn("macOS Apple silicon archive", body)
        self.assertNotIn("Unreleased", body)
        self.assertNotIn("older", body)
        self.assertNotIn("https://", body)
        self.assertTrue(body.endswith("\n"))

    def test_missing_or_empty_section_fails(self):
        with self.assertRaises(ReleaseCheckError) as raised:
            changelog_section(CHANGELOG, "0.2.0")
        self.assertIn("No release is created", str(raised.exception))
        empty = "# Changelog\n\n## [0.1.0] - 2026-10-31\n\n## [Unreleased]\n"
        with self.assertRaises(ReleaseCheckError):
            changelog_section(empty, "0.1.0")


class CommandTests(unittest.TestCase):
    def test_cli_mismatch_exits_without_writing_env(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "release.env"
            completed = subprocess.run(
                [
                    sys.executable,
                    str(ROOT / "scripts/release_version.py"),
                    "check-tag",
                    "--tag",
                    "v0.2.0",
                    "--radiko",
                    str(ROOT / "crates/radiko/Cargo.toml"),
                    "--client",
                    str(ROOT / "crates/radiko-client/Cargo.toml"),
                    "--env-file",
                    str(path),
                ],
                check=False,
                capture_output=True,
                text=True,
            )
            self.assertEqual(completed.returncode, 1)
            self.assertIn("No release is created", completed.stderr)
            self.assertEqual(completed.stdout, "")
            self.assertFalse(path.exists())

    def test_cli_prerelease_suffix_writes_core_version(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "release.env"
            completed = subprocess.run(
                [
                    sys.executable,
                    str(ROOT / "scripts/release_version.py"),
                    "check-tag",
                    "--tag",
                    "v0.1.0-test.1",
                    "--radiko",
                    str(ROOT / "crates/radiko/Cargo.toml"),
                    "--client",
                    str(ROOT / "crates/radiko-client/Cargo.toml"),
                    "--env-file",
                    str(path),
                ],
                check=False,
                capture_output=True,
                text=True,
            )
            self.assertEqual(completed.returncode, 0, completed.stderr)
            self.assertIn("PRERELEASE=true", path.read_text())
            self.assertIn("RELEASE_VERSION=0.1.0", path.read_text())


class RepositoryTests(unittest.TestCase):
    def test_checked_in_crates_match_v0_1_0_and_reject_v0_2_0(self):
        version = shared_cargo_version(
            (ROOT / "crates/radiko/Cargo.toml").read_text(),
            (ROOT / "crates/radiko-client/Cargo.toml").read_text(),
        )
        self.assertEqual(version, "0.1.0")
        stable = parse_tag("v0.1.0", version)
        self.assertFalse(stable.prerelease)
        preview = parse_tag("v0.1.0-test.1", version)
        self.assertTrue(preview.prerelease)
        self.assertEqual(preview.cargo_version, "0.1.0")
        with self.assertRaises(ReleaseCheckError) as raised:
            parse_tag("v0.2.0", version)
        self.assertIn("No release is created", str(raised.exception))

    def test_checked_in_changelog_has_unreleased_and_the_0_1_0_section(self):
        text = (ROOT / "CHANGELOG.md").read_text()
        self.assertIn("Keep a Changelog", text)
        self.assertIn("## [Unreleased]", text)
        body = changelog_section(text, "0.1.0")
        self.assertIn("## [0.1.0]", body)
        self.assertNotIn("## [Unreleased]", body)
        self.assertIn("aarch64-apple-darwin", body)
        self.assertIn("v0.1.0-test.1", body)


if __name__ == "__main__":
    unittest.main()
