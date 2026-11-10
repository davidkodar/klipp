#!/usr/bin/env python3
"""The Flathub metainfo (assets/io.github.davidkodar.Klipp.metainfo.xml):
valid for AppStream's strict check, its releases the repository's tags,
and the parts Flathub asks for there."""

from pathlib import Path
import re
import shutil
import subprocess
import unittest
import xml.etree.ElementTree as ET


REPO = Path(__file__).resolve().parents[2]
METAINFO = REPO / "assets/io.github.davidkodar.Klipp.metainfo.xml"


def tags():
    """The release tags, `v0.3.4` or `v0.1.0-rc.1`, with their commits'
    dates; none outside a checkout with tags."""
    try:
        out = subprocess.run(["git", "-C", str(REPO), "tag", "-l", "v*", "--format=%(refname:short) %(*committerdate:short)%(committerdate:short)"],
                             text=True, capture_output=True, check=True).stdout
    except (OSError, subprocess.CalledProcessError):
        return {}
    found = {}
    for line in out.splitlines():
        name, date = line.split()
        if re.fullmatch(r"v\d+\.\d+\.\d+(-rc\.\d+)?", name):
            found[name[1:]] = date
    return found


def as_tag(version):
    """A metainfo version as its tag names it: AppStream orders `~rc`
    before the release, the tag says `-rc`."""
    return version.replace("~", "-")


class MetainfoTests(unittest.TestCase):
    def setUp(self):
        self.root = ET.parse(METAINFO).getroot()

    def test_passes_appstreams_strict_check(self):
        if shutil.which("appstreamcli") is None:
            self.skipTest("appstreamcli is not installed")
        result = subprocess.run(["appstreamcli", "validate", "--strict", "--no-net", "--explain", str(METAINFO)],
                                text=True, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_releases_are_the_tags_newest_first(self):
        found = tags()
        if not found:
            self.skipTest("no release tags in this checkout")
        releases = self.root.findall("releases/release")
        listed = {as_tag(r.get("version")): r.get("date") for r in releases}
        self.assertEqual(listed, found, "each tag a release, on its commit's date")
        for r in releases:
            development = "-rc." in as_tag(r.get("version"))
            self.assertEqual(r.get("type") == "development", development, r.get("version"))
        dates = [r.get("date") for r in releases]
        self.assertEqual(dates, sorted(dates, reverse=True), "newest first")

    def test_has_what_flathub_asks_for(self):
        r = self.root
        self.assertEqual(r.findtext("id"), "io.github.davidkodar.Klipp")
        self.assertEqual(r.find("launchable").text, "io.github.davidkodar.Klipp.desktop")
        self.assertTrue(r.find("developer").get("id"))
        self.assertIsNotNone(r.find("content_rating[@type='oars-1.1']"))
        urls = {u.get("type") for u in r.findall("url")}
        self.assertLessEqual({"homepage", "bugtracker", "vcs-browser"}, urls)
        schemes = {c.get("scheme_preference") for c in r.findall("branding/color[@type='primary']")}
        self.assertEqual(schemes, {"light", "dark"})
        self.assertIsNotNone(r.find("requires/display_length"))
        # Screenshots wait for free-licensed media only.
        self.assertIsNone(r.find("screenshots"))


if __name__ == "__main__":
    unittest.main()
