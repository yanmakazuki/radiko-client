"""Offline checks for catalog RSS units. No radiko account, network, or audio."""
import sys
import unittest

from verify_tui_catalog import child_maxrss_bytes


class ChildRssTests(unittest.TestCase):
    def test_linux_kib_are_scaled_to_bytes(self):
        # 122252 KiB is about 119 MiB. Reporting the raw value would be 1024 times small.
        self.assertEqual(child_maxrss_bytes(122252, 'linux'), 122252 * 1024)
        self.assertEqual(child_maxrss_bytes(0, 'linux'), 0)

    def test_macos_bytes_are_unchanged(self):
        self.assertEqual(child_maxrss_bytes(122252, 'darwin'), 122252)
        self.assertEqual(child_maxrss_bytes(125186048, 'darwin'), 125186048)

    def test_default_follows_the_host_platform(self):
        expected = 10 * 1024 if sys.platform == 'linux' else 10
        self.assertEqual(child_maxrss_bytes(10), expected)


if __name__ == '__main__':
    unittest.main()
