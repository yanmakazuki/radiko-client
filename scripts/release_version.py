#!/usr/bin/env python3
"""Decide whether a v* tag may publish a release, and copy its changelog section.

The release workflow calls this script and must not run ``gh release create``
when it exits non-zero. Nothing in this file talks to GitHub.

Cargo.toml
    ``crates/radiko`` and ``crates/radiko-client`` must carry the same
    ``MAJOR.MINOR.PATCH`` version, for example ``0.1.0``. A prerelease suffix
    or build metadata in either file is rejected. The two versions must match
    each other.

Tag
    ``vX.Y.Z``
        Stable GitHub release when ``X.Y.Z`` is exactly that Cargo.toml version.
    ``vX.Y.Z-<semver prerelease>``
        GitHub prerelease when ``X.Y.Z`` is exactly that Cargo.toml version.
        ``v0.1.0-test.1`` is the example used here: both crates stay at
        ``0.1.0``, and the suffix is not written into Cargo.toml. Release notes
        still come from the ``## [0.1.0]`` section of CHANGELOG.md.
    anything else
        ``ReleaseCheckError``. Callers must not create a release. This includes
        ``v0.2.0`` while the crates are ``0.1.0``, a suffix whose core does not
        match (``v0.2.0-test.1``), build metadata (``v0.1.0+gha``), and a bare
        ``v0.1``.

Assets
    ``radiko-client-<tag version without v>-aarch64-apple-darwin.tar.gz``
    and the same name with ``.sha256``. The workflow does not build Linux or
    Intel Mac artifacts.
"""

import argparse
import re
import sys
from pathlib import Path

TARGET = "aarch64-apple-darwin"
_CORE = re.compile(r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$")
# SemVer prerelease identifier: a numeric identifier without leading zeros,
# or an alphanumeric identifier that contains a letter or hyphen.
_PRE_ID = re.compile(r"^(?:0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*)$")
_HEADING = re.compile(r"^## \[([^\]]+)\](?:\s.*)?$")
_LINK_DEF = re.compile(r"^\[[^\]]+\]:\s+\S")


class ReleaseCheckError(Exception):
    """The tag must not be published. Callers must not create a release."""


class TagDecision(object):
    def __init__(self, cargo_version, tag_version, prerelease):
        self.cargo_version = cargo_version
        self.tag_version = tag_version
        self.prerelease = prerelease

    def env_text(self):
        flag = "true" if self.prerelease else "false"
        lines = (
            "RELEASE_VERSION=%s" % self.cargo_version,
            "TAG_VERSION=%s" % self.tag_version,
            "PRERELEASE=%s" % flag,
            "ASSET_BASENAME=%s" % asset_basename(self.tag_version),
        )
        return "\n".join(lines) + "\n"


def cargo_package_version(text, label):
    """Return the ``[package]`` version. Ignore versions under other tables."""
    in_package = False
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith("[") and stripped.endswith("]"):
            in_package = stripped == "[package]"
            continue
        if not in_package:
            continue
        if stripped.startswith("version"):
            match = re.fullmatch(r'version\s*=\s*"([^"]*)"', stripped)
            if match is None:
                raise ReleaseCheckError("%s has an unreadable package version" % label)
            version = match.group(1)
            if _CORE.fullmatch(version) is None:
                raise ReleaseCheckError(
                    "%s version %s must be MAJOR.MINOR.PATCH with no suffix. "
                    "No release is created." % (label, version)
                )
            return version
    raise ReleaseCheckError("%s has no [package] version. No release is created." % label)


def shared_cargo_version(radiko_text, client_text):
    radiko = cargo_package_version(radiko_text, "crates/radiko/Cargo.toml")
    client = cargo_package_version(client_text, "crates/radiko-client/Cargo.toml")
    if radiko != client:
        raise ReleaseCheckError(
            "crate versions differ (%s and %s). No release is created." % (radiko, client)
        )
    return radiko


def _prerelease_suffix_ok(suffix):
    if suffix == "":
        return False
    return all(_PRE_ID.fullmatch(part) for part in suffix.split("."))


def parse_tag(tag, cargo_version):
    """Return a decision, or raise when the tag must not be published."""
    if not isinstance(tag, str) or not tag.startswith("v") or tag.startswith("vv"):
        raise ReleaseCheckError("refusing tag %r: expected a single leading v. No release is created." % (tag,))
    rest = tag[1:]
    if rest == "" or "/" in rest or "\n" in rest or " " in rest:
        raise ReleaseCheckError("refusing tag %s. No release is created." % tag)
    if "+" in rest:
        raise ReleaseCheckError(
            "refusing tag %s: build metadata is not accepted. No release is created." % tag
        )
    if "-" in rest:
        core, suffix = rest.split("-", 1)
        prerelease = True
    else:
        core, suffix = rest, ""
        prerelease = False
    if _CORE.fullmatch(core) is None:
        raise ReleaseCheckError(
            "refusing tag %s: %s is not MAJOR.MINOR.PATCH. No release is created." % (tag, core or "(empty)")
        )
    if core != cargo_version:
        raise ReleaseCheckError(
            "refusing tag %s: %s does not match Cargo.toml version %s. No release is created."
            % (tag, core, cargo_version)
        )
    if prerelease and not _prerelease_suffix_ok(suffix):
        raise ReleaseCheckError(
            "refusing tag %s: %s is not a SemVer prerelease suffix. "
            "The Cargo.toml version stays %s. No release is created."
            % (tag, suffix, cargo_version)
        )
    return TagDecision(cargo_version, rest, prerelease)


def asset_basename(tag_version):
    if _CORE.fullmatch(tag_version) is None and "-" not in tag_version:
        raise ReleaseCheckError("refusing asset name for %s. No release is created." % tag_version)
    name = "radiko-client-%s-%s" % (tag_version, TARGET)
    if not name.endswith("-" + TARGET) or "/" in name:
        raise ReleaseCheckError("refusing asset name %s. No release is created." % name)
    return name


def changelog_section(text, version):
    """Return the ``## [version]`` section, including its heading.

    The section stops before the next ``## [`` heading or a Keep a Changelog
    link definition. ``version`` is the Cargo.toml core (``0.1.0``), including
    when the tag is a prerelease such as ``v0.1.0-test.1``.
    """
    if _CORE.fullmatch(version) is None:
        raise ReleaseCheckError(
            "changelog lookup %s is not MAJOR.MINOR.PATCH. No release is created." % version
        )
    lines = text.splitlines()
    start = None
    for index, line in enumerate(lines):
        match = _HEADING.match(line)
        if match and match.group(1) == version:
            start = index
            break
    if start is None:
        raise ReleaseCheckError(
            "CHANGELOG.md has no ## [%s] section. No release is created." % version
        )
    end = len(lines)
    for index in range(start + 1, len(lines)):
        if _HEADING.match(lines[index]) or _LINK_DEF.match(lines[index]):
            end = index
            break
    body_lines = lines[start:end]
    while body_lines and body_lines[-1].strip() == "":
        body_lines.pop()
    content = [line for line in body_lines[1:] if line.strip()]
    if not content:
        raise ReleaseCheckError(
            "CHANGELOG.md section ## [%s] is empty. No release is created." % version
        )
    return "\n".join(body_lines) + "\n"


def write_env(decision, path):
    target = Path(path)
    temporary = target.with_name(target.name + ".tmp")
    temporary.write_text(decision.env_text())
    temporary.replace(target)


def _read(path):
    return Path(path).read_text()


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command")

    check = commands.add_parser("check-tag")
    check.add_argument("--tag", required=True)
    check.add_argument("--radiko", default="crates/radiko/Cargo.toml")
    check.add_argument("--client", default="crates/radiko-client/Cargo.toml")
    check.add_argument("--env-file")

    notes = commands.add_parser("changelog")
    notes.add_argument("--version", required=True)
    notes.add_argument("--changelog", default="CHANGELOG.md")
    notes.add_argument("--output")

    args = parser.parse_args(argv)
    if args.command is None:
        parser.error("choose check-tag or changelog")
    try:
        if args.command == "check-tag":
            version = shared_cargo_version(_read(args.radiko), _read(args.client))
            decision = parse_tag(args.tag, version)
            text = decision.env_text()
            if args.env_file:
                write_env(decision, args.env_file)
            sys.stdout.write(text)
        else:
            body = changelog_section(_read(args.changelog), args.version)
            if args.output:
                Path(args.output).write_text(body)
            else:
                sys.stdout.write(body)
    except ReleaseCheckError as error:
        sys.stderr.write("release check failed: %s\n" % error)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
