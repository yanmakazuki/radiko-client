"""Offline checks for the TUI smoke scanner. No radiko, account, terminal, or audio."""
import unittest

from smoke_tui import network_sockets, output_problems


class OutputProblemTests(unittest.TestCase):
    def test_rejects_alsa_library_text_and_a_rust_panic(self):
        self.assertEqual(output_problems('ALSA lib pcm_dmix.c'.encode()), ['ALSA lib'])
        self.assertEqual(output_problems(b"thread 'main' panicked at tui.rs"), ['panicked'])
        self.assertEqual(
            output_problems('ALSA lib\npanicked at'.encode()),
            ['ALSA lib', 'panicked'],
        )

    def test_allows_the_offline_program_screen(self):
        text = 'タイムフリー / TBS\nオフライン番組\n取得完了\nオフライン確認'
        self.assertEqual(output_problems(text.encode()), [])
        self.assertEqual(output_problems(b''), [])


class NetworkSocketTests(unittest.TestCase):
    def test_this_process_reports_a_list_on_linux(self):
        import os
        sockets = network_sockets(os.getpid())
        if sockets is None:
            self.skipTest('/proc is unavailable')
        self.assertIsInstance(sockets, list)


if __name__ == '__main__':
    unittest.main()
