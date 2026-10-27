"""Offline checks for catalog RSS units. No radiko account, network, or audio."""
import unittest

from verify_tui_catalog import child_maxrss_bytes


class ChildRssTests(unittest.TestCase):
    def test_linux_kib_are_scaled_to_bytes(self):
        # 122252 KiB is about 119 MiB. Reporting the raw value would be 1024 times small.
        self.assertEqual(child_maxrss_bytes(122252, 'linux'), 122252 * 1024)
        self.assertEqual(child_maxrss_bytes(0, 'linux'), 0)

    def test_darwin_bytes_stay_unchanged(self):
        self.assertEqual(child_maxrss_bytes(122252, 'darwin'), 122252)
        self.assertEqual(child_maxrss_bytes(125186048, 'darwin'), 125186048)


if __name__ == '__main__':
    unittest.main()
