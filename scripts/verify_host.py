#!/usr/bin/env python3
"""Print host-specific verification commands. Does not open a device or record audio.

macOS session and idle-inhibit commands stay on the historical forms.
Linux session follows the directories crate (XDG data home, otherwise ~/.local/share).
PipeWire and PulseAudio recording commands are printed only; this process does not run them.
"""
import argparse
import os
from pathlib import Path
import shlex
import sys


def default_session_path(platform, home, xdg_data_home=None):
    """Match SessionStore's default session.json for macOS and Linux."""
    home = Path(home)
    if platform == 'darwin':
        return home / 'Library/Application Support/jp.radiko-client.radiko-client/session.json'
    if platform == 'linux':
        base = Path(xdg_data_home) if xdg_data_home else home / '.local' / 'share'
        return base / 'radiko-client' / 'session.json'
    raise ValueError('unsupported platform')


def idle_inhibit_prefix(platform):
    """Prefix that blocks idle sleep only while the verification process runs."""
    if platform == 'darwin':
        return ['caffeinate', '-i']
    if platform == 'linux':
        return ['systemd-inhibit', '--what=idle', '--mode=block']
    raise ValueError('unsupported platform')


def classify_audio_server(pactl_info='', pw_info=''):
    """Tell PipeWire from PulseAudio using command output. PipeWire wins when both names appear."""
    text = '%s\n%s' % (pactl_info or '', pw_info or '')
    if 'PipeWire' in text:
        return 'pipewire'
    if 'pulseaudio' in text.lower():
        return 'pulse'
    return None


def list_sinks_command(server):
    if server == 'pipewire':
        return ['pw-cli', 'list-objects', 'Node']
    if server == 'pulse':
        return ['pactl', 'list', 'short', 'sinks']
    raise ValueError('server must be pipewire or pulse')


def default_sink_command(server):
    """PulseAudio and pipewire-pulse both answer pactl. Pure PipeWire without that protocol does not."""
    if server in ('pipewire', 'pulse'):
        return ['pactl', 'get-default-sink']
    raise ValueError('server must be pipewire or pulse')


def monitor_name(sink):
    if not isinstance(sink, str) or sink == '' or any(character.isspace() for character in sink):
        raise ValueError('sink name must be a single token')
    if sink.endswith('.monitor'):
        return sink
    return sink + '.monitor'


def monitor_record_command(server, sink, destination):
    """Argv for a monitor recording. The caller decides whether to run it."""
    monitor = monitor_name(sink)
    destination = str(destination)
    if server == 'pipewire':
        return ['pw-record', '--target', monitor, '--rate', '48000', '--channels', '2', destination]
    if server == 'pulse':
        return ['parecord', '--device=' + monitor, '--file-format=wav',
                '--rate=48000', '--channels=2', destination]
    raise ValueError('server must be pipewire or pulse')


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='command', required=True)
    sub.add_parser('session', help='print the default session.json path for this OS')
    sub.add_parser('inhibit', help='print the idle-sleep inhibit prefix for this OS')
    sinks = sub.add_parser('sinks', help='print the sink-list command')
    sinks.add_argument('--server', required=True, choices=('pipewire', 'pulse'))
    default_sink = sub.add_parser('default-sink', help='print the default-sink command')
    default_sink.add_argument('--server', required=True, choices=('pipewire', 'pulse'))
    record = sub.add_parser('record', help='print a monitor-record command without running it')
    record.add_argument('--server', required=True, choices=('pipewire', 'pulse'))
    record.add_argument('--sink', required=True)
    record.add_argument('--destination', required=True)
    args = parser.parse_args(argv)
    if args.command == 'session':
        print(default_session_path(sys.platform, Path.home(), os.environ.get('XDG_DATA_HOME') or None))
    elif args.command == 'inhibit':
        print(shlex.join(idle_inhibit_prefix(sys.platform)))
    elif args.command == 'sinks':
        print(shlex.join(list_sinks_command(args.server)))
    elif args.command == 'default-sink':
        print(shlex.join(default_sink_command(args.server)))
    else:
        print(shlex.join(monitor_record_command(args.server, args.sink, args.destination)))
    return 0


if __name__ == '__main__':
    raise SystemExit(main(sys.argv[1:]))
