"""Offline checks for host verification commands. No audio device and no recording."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from verify_host import (
    classify_audio_server,
    default_session_path,
    default_sink_command,
    idle_inhibit_prefix,
    list_sinks_command,
    monitor_name,
    monitor_record_command,
)


class SessionPathTests(unittest.TestCase):
    def test_macos_path_ignores_xdg_and_keeps_the_application_support_layout(self):
        path = default_session_path('darwin', '/Users/example', xdg_data_home='/tmp/data')
        self.assertEqual(
            path,
            Path('/Users/example/Library/Application Support/jp.radiko-client.radiko-client/session.json'),
        )

    def test_linux_uses_xdg_data_home_when_it_is_set(self):
        path = default_session_path('linux', '/home/example', xdg_data_home='/tmp/data')
        self.assertEqual(path, Path('/tmp/data/radiko-client/session.json'))

    def test_linux_without_xdg_uses_local_share(self):
        path = default_session_path('linux', '/home/example', xdg_data_home=None)
        self.assertEqual(path, Path('/home/example/.local/share/radiko-client/session.json'))
        self.assertEqual(default_session_path('linux', '/home/example', xdg_data_home=''), path)


class InhibitTests(unittest.TestCase):
    def test_macos_prefix_stays_caffeinate(self):
        self.assertEqual(idle_inhibit_prefix('darwin'), ['caffeinate', '-i'])

    def test_linux_prefix_blocks_idle_only_for_the_child(self):
        self.assertEqual(idle_inhibit_prefix('linux'), ['systemd-inhibit', '--what=idle', '--mode=block'])


class AudioServerTests(unittest.TestCase):
    def test_pipewire_pulse_compat_is_classified_as_pipewire(self):
        text = 'Server Name: PulseAudio (on PipeWire 1.0.5)\nServer Version: 15.0.0\n'
        self.assertEqual(classify_audio_server(text), 'pipewire')

    def test_pulseaudio_without_pipewire_is_pulse(self):
        text = 'Server Name: pulseaudio\nServer Version: 16.1\n'
        self.assertEqual(classify_audio_server(text), 'pulse')

    def test_pw_cli_banner_is_pipewire_even_without_pactl(self):
        self.assertEqual(classify_audio_server('', 'id: 0\ntype: PipeWire:Interface:Core\n'), 'pipewire')

    def test_unknown_output_is_not_a_server(self):
        self.assertIsNone(classify_audio_server('no audio server'))

    def test_sink_commands_differ_by_server_and_share_the_pulse_default_query(self):
        self.assertEqual(list_sinks_command('pipewire'), ['pw-cli', 'list-objects', 'Node'])
        self.assertEqual(list_sinks_command('pulse'), ['pactl', 'list', 'short', 'sinks'])
        self.assertEqual(default_sink_command('pipewire'), ['pactl', 'get-default-sink'])
        self.assertEqual(default_sink_command('pulse'), ['pactl', 'get-default-sink'])

    def test_monitor_name_and_record_commands(self):
        self.assertEqual(monitor_name('null'), 'null.monitor')
        self.assertEqual(monitor_name('null.monitor'), 'null.monitor')
        with self.assertRaises(ValueError):
            monitor_name('bad sink')
        pipewire = monitor_record_command('pipewire', 'null', '/tmp/monitor.wav')
        pulse = monitor_record_command('pulse', 'null', '/tmp/monitor.wav')
        self.assertEqual(pipewire[:4], ['pw-record', '--target', 'null.monitor', '--rate'])
        self.assertEqual(pulse[0], 'parecord')
        self.assertIn('--device=null.monitor', pulse)
        self.assertNotIn('parecord', pipewire)
        self.assertNotIn('pw-record', pulse)
        with self.assertRaises(ValueError):
            monitor_record_command('alsa', 'null', '/tmp/monitor.wav')


class CommandPrinterTests(unittest.TestCase):
    def test_session_and_inhibit_follow_this_host(self):
        script = Path(__file__).with_name('verify_host.py')
        session = subprocess.run([sys.executable, str(script), 'session'],
                                 capture_output=True, text=True, timeout=5)
        inhibit = subprocess.run([sys.executable, str(script), 'inhibit'],
                                 capture_output=True, text=True, timeout=5)
        self.assertEqual(session.returncode, 0)
        self.assertEqual(inhibit.returncode, 0)
        expected = default_session_path(sys.platform, Path.home(), os.environ.get('XDG_DATA_HOME') or None)
        self.assertEqual(session.stdout.strip(), str(expected))
        prefix = idle_inhibit_prefix(sys.platform)
        self.assertTrue(inhibit.stdout.startswith(prefix[0]))

    def test_record_prints_the_command_and_does_not_create_the_file(self):
        script = Path(__file__).with_name('verify_host.py')
        with tempfile.TemporaryDirectory() as directory:
            destination = Path(directory) / 'monitor.wav'
            result = subprocess.run(
                [sys.executable, str(script), 'record', '--server', 'pulse',
                 '--sink', 'null', '--destination', str(destination)],
                capture_output=True, text=True, timeout=5)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn('parecord', result.stdout)
            self.assertIn('null.monitor', result.stdout)
            self.assertFalse(destination.exists())
            listed = subprocess.run(
                [sys.executable, str(script), 'sinks', '--server', 'pipewire'],
                capture_output=True, text=True, timeout=5)
            self.assertEqual(listed.stdout.strip(), 'pw-cli list-objects Node')


if __name__ == '__main__':
    unittest.main()
