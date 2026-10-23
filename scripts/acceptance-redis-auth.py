"""Validate Redis authentication with a fresh workspace-owned component and data."""
import copy
import importlib.util
import json
import os
from pathlib import Path
import secrets
import shutil
import socket

spec = importlib.util.spec_from_file_location('web', Path(__file__).with_name('acceptance-web-entries.py'))
web = importlib.util.module_from_spec(spec)
spec.loader.exec_module(web)


def main():
    web.checked(web.ROOT).mkdir(parents=True)
    source = web.checked(web.WORKSPACE / 'bin/redis/redis-7.4.3')
    for directory, folders, files in os.walk(source, followlinks=False):
        web.checked(directory)
        for name in folders + files:
            web.checked(Path(directory) / name)
    target = web.checked(web.ROOT / 'bin/redis' / source.name)
    target.parent.mkdir(parents=True)
    shutil.copytree(source, target)
    session = web.Session()
    results = []
    passwords = [secrets.token_hex(24) + ' /|${}中文', secrets.token_hex(24), secrets.token_hex(24) + '\u00a0']

    def running():
        return next(r for r in session.snapshot['rows'] if r['id'] == 'redis')['running']

    def command(*args, password=None):
        # The manager must first verify ownership of this fixture's Redis listener.
        assert running()
        with socket.create_connection(('127.0.0.1', port), timeout=5) as client:
            reader = client.makefile('rb')
            def send(values):
                values = [v.encode('utf-8') for v in values]
                client.sendall(b'*%d\r\n' % len(values) + b''.join(
                    b'$%d\r\n' % len(v) + v + b'\r\n' for v in values))
                reply = reader.readline()
                if reply.startswith(b'$') and int(reply[1:]) >= 0:
                    reply += reader.read(int(reply[1:]) + 2)
                return reply
            if password is not None:
                assert send(['AUTH', password]) == b'+OK\r\n', 'Authentication failed'
            return send(args)

    def save(password, separator=' '):
        text = base + '\nrequirepass' + separator + json.dumps(password, ensure_ascii=False) + '\n'
        return session.call('save_config', id='redis', text=text)

    try:
        session.call('scan')
        config = copy.deepcopy(session.snapshot['config'])
        with socket.socket() as reserved:
            reserved.bind(('127.0.0.1', 0))
            port = reserved.getsockname()[1]
        config['redis']['port'] = port
        session.call('settings', config=config)
        base = session.call('read_config', id='redis')['text']
        save(passwords[0], separator='\t')
        session.call('start', id='redis')
        assert command('PING').startswith(b'-NOAUTH')
        assert command('PING', password=passwords[0]) == b'+PONG\r\n'
        assert command('SET', 'fixture', 'preserved', password=passwords[0]) == b'+OK\r\n'
        results.append('Password with spaces, Unicode and path-like symbols saves and authenticates')
        state_path = web.checked(web.ROOT / 'runtime/state/processes.json')
        identities = [r['identity'] for r in json.loads(state_path.read_bytes())['records']]
        session.call('scan')
        assert identities == [r['identity'] for r in json.loads(state_path.read_bytes())['records']]
        assert running()
        results.append('Component refresh preserves the running Redis process identity')
        save(passwords[1])
        assert running()
        assert command('GET', 'fixture', password=passwords[1]) == b'$9\r\npreserved\r\n'
        session.call('restart', id='redis')
        assert command('PING', password=passwords[1]) == b'+PONG\r\n'
        results.append('Password rotation and restart use authenticated normal shutdown and preserve data')
        # Closing the CLI leaves its owned Redis alive for process-identity recovery.
        session.process.stdin.close()
        session.process.wait(timeout=15)
        session = web.Session()
        session.call('scan')
        assert running()
        session.call('stop', id='redis')
        session.call('start', id='redis')
        assert command('GET', 'fixture', password=passwords[1]) == b'$9\r\npreserved\r\n'
        results.append('Manager reopen recovers the protected running credential and stops normally')
        invalid = base + '\nrequirepass ' + json.dumps(passwords[0]) + '\nnot-a-redis-setting yes\n'
        error = session.call('save_config', expect=False, id='redis', text=invalid)['error']
        assert all(p not in error for p in passwords)
        assert running()
        assert command('PING', password=passwords[1]) == b'+PONG\r\n'
        results.append('Invalid native configuration restores the previous password and running service')
        save('')
        assert command('GET', 'fixture') == b'$9\r\npreserved\r\n'
        results.append('Empty requirepass disables authentication without losing data')
        session.call('save_config', id='redis', text=base + '\nrequirepass ' + passwords[2] + '\n')
        assert command('PING', password=passwords[2]) == b'+PONG\r\n'
        save('')
        results.append('Redis whitespace delimiters and unquoted trailing Unicode whitespace match native authentication')
        session.call('stop_all')
        # Only native active/candidate configuration may contain a password.
        for folder in ['runtime/state', 'runtime/path-backups', 'logs', 'config/services']:
            root = web.checked(web.ROOT / folder)
            if not root.exists():
                continue
            for path in root.rglob('*'):
                web.checked(path)
                if not path.is_file() or path.suffix == '.conf':
                    continue
                data = path.read_bytes()
                for password in passwords:
                    assert password.encode('utf-8') not in data, 'Credential in recovery or log file'
                if path.name == 'processes.json':
                    state = json.loads(data)
                    assert all(bytes(v).startswith(b'FLARAGON-DPAPI-1\0') for v in state['applied_files'].values())
        results.append('Runtime snapshots, backup files and logs contain no plaintext test passwords')
    finally:
        try:
            session.call('stop_all')
            assert not any(r['running'] for r in session.snapshot['rows'])
        finally:
            session.process.stdin.close()
            session.process.wait(timeout=15)
    evidence = dict(environment=str(web.ROOT), executable=str(web.EXE), component='redis-windows 7.4.3 MSYS2 x64', results=results, stopped=True)
    web.write(web.WORKSPACE / 'testwork/evidence/redis-auth.json', json.dumps(evidence, ensure_ascii=False, indent=2))
    print(json.dumps(evidence, ensure_ascii=False, indent=2))


if __name__ == '__main__':
    main()
