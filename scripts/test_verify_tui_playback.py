"""Offline tests for measurement parsing; no radiko account, network, or audio."""
import unittest
import os
import pty
import subprocess
import sys
import datetime

from verify_tui_playback import Terminal, wait_exit, target_matches, failure_diagnostics


class TerminalTests(unittest.TestCase):
    def test_selected_detail_cannot_substitute_for_the_playing_slot(self):
        start = datetime.datetime.fromisoformat('2026-10-15T12:00:00+09:00')
        end = start + datetime.timedelta(hours=1)
        selected = '10/15 12:00〜10/15 13:00 JST'
        wrong = selected + '\n再生中: same title / OBS / TimeFree\n' + selected
        self.assertFalse(target_matches(wrong, 'TBS', start, end))
        wrong = selected + '\n再生中: same title / TBS / TimeFree\n10/15 11:00〜10/15 12:00 JST'
        self.assertFalse(target_matches(wrong, 'TBS', start, end))
        self.assertTrue(target_matches('再生中: title / TBS / TimeFree\n' + selected, 'TBS', start, end))

    def test_failure_diagnostics_ignore_program_text_and_never_return_raw_notice(self):
        body = '再生停止: 配信取得: signed-url-secret\n' + '\n'.join(['body'] * 9)
        self.assertFalse(failure_diagnostics(body)['notice_visible'])
        footer = '\n'.join(['body'] * 10 + ['│再生中: 再生停止: title / TBS / TimeFree',
            '│再生停止: 配信取得: 配信サーバーがHTTP 503を返しました', '│', '│'])
        self.assertEqual(failure_diagnostics(footer), dict(notice_visible=True, error_kind='source_other', http_status=503))
        unknown = '\n'.join(['body'] * 10 + ['│再生停止: sensitive-unrecognized-text', '│'])
        self.assertNotIn('sensitive-unrecognized-text', str(failure_diagnostics(unknown)))

    def test_every_utf8_and_csi_split_preserves_japanese_and_restoration(self):
        content = '\x1b[2J\x1b[2;3H再生位置\x1b[?1049l'.encode()
        for split in range(len(content) + 1):
            with self.subTest(split=split):
                terminal = Terminal(-1)
                terminal.feed(content[:split])
                terminal.screen()
                terminal.feed(content[split:])
                self.assertIn('再生位置', terminal.screen())
                self.assertTrue(terminal.restored)
                self.assertEqual(terminal.pending, '')

    def test_relative_cursor_updates_continue_across_screen_reads(self):
        terminal = Terminal(-1)
        terminal.feed(b'\x1b[3;5H12345')
        first = terminal.screen()
        terminal.feed(b'\x1b[2DXX')
        self.assertEqual(first.splitlines()[2][4:9], '12345')
        self.assertEqual(terminal.screen().splitlines()[2][4:9], '123XX')
        terminal.feed(b'\x1b[3;5H\x1b[K')
        self.assertEqual(terminal.screen().splitlines()[2].strip(), '')

    def test_osc_and_fragmented_private_sequences_are_not_screen_content(self):
        terminal = Terminal(-1)
        for chunk in [b'\x1b]0;title', b'\x1b', b'\\visible', b'\x1b[?104', b'9l']:
            terminal.feed(chunk)
        self.assertNotIn('title', terminal.screen())
        self.assertTrue(terminal.screen().startswith('visible'))
        self.assertTrue(terminal.restored)

    def test_incomplete_control_sequence_is_bounded(self):
        terminal = Terminal(-1)
        terminal.feed(b'\x1b[')
        with self.assertRaises(RuntimeError):
            terminal.feed(b'1' * 131072)

    def test_exit_wait_drains_more_than_a_pty_buffer_before_joining(self):
        master, slave = pty.openpty()
        proc = subprocess.Popen([sys.executable, '-c', "import os; os.write(1, b'x' * 262144)"],
                                stdin=slave, stdout=slave, stderr=slave)
        try:
            wait_exit(proc, Terminal(master), timeout=5)
            self.assertEqual(proc.returncode, 0)
        finally:
            if proc.poll() is None:
                proc.kill()
                proc.wait()
            os.close(master)
            os.close(slave)


if __name__ == '__main__':
    unittest.main()
