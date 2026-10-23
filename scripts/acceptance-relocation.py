"""Verify stopped-directory relocation using only new workspace-local service data."""
import copy
import json
import os
from pathlib import Path
import shutil
import socket
import stat
import subprocess
import uuid

WORKSPACE = Path(__file__).resolve().parent.parent
BASE = WORKSPACE / 'testwork' / ('relocation-' + uuid.uuid4().hex[:12])
ROOT = BASE / 'original environment'
EXE = WORKSPACE / 'target/x86_64-pc-windows-msvc/release/flaragon-cli.exe'
RESULTS = []
ENV = dict(SystemRoot=os.environ.get('SystemRoot', r'C:\Windows'),
           FLARAGON_DEV_ROOT=str(WORKSPACE), TEMP=str(WORKSPACE / '.cache/tmp'),
           TMP=str(WORKSPACE / '.cache/tmp'))


def checked(path):
    path = Path(os.path.abspath(path))
    if not path.is_relative_to(WORKSPACE):
        raise RuntimeError('Path outside workspace')
    for part in reversed((path, *path.parents)):
        if part.exists() and part.lstat().st_file_attributes & stat.FILE_ATTRIBUTE_REPARSE_POINT:
            raise RuntimeError('Reparse point rejected')
    return path


def passed(name):
    RESULTS.append(dict(name=name, passed=True))
    print('PASS', name, flush=True)


def port():
    with socket.socket() as listener:
        listener.bind(('127.0.0.1', 0))
        return listener.getsockname()[1]


class Session:
    def __init__(self):
        self.proc = subprocess.Popen([str(checked(EXE)), str(checked(ROOT))],
                                     cwd=ROOT, env=ENV, stdin=subprocess.PIPE,
                                     stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                     text=True, encoding='utf-8',
                                     creationflags=subprocess.CREATE_NO_WINDOW)
        self.snapshot = None

    def call(self, action, **args):
        self.proc.stdin.write(json.dumps(dict(action=action, **args), ensure_ascii=False) + '\n')
        self.proc.stdin.flush()
        line = self.proc.stdout.readline()
        if not line:
            raise RuntimeError('CLI exited: ' + self.proc.stderr.read())
        result = json.loads(line)
        assert result['ok'], result.get('error', action)
        self.snapshot = result['snapshot']
        return result

    def close(self):
        self.proc.stdin.close()
        assert self.proc.wait(timeout=20) == 0


def mysql(session, package, db_port, sql):
    assert any(r['id'].startswith('mysql:') and r['running'] and str(db_port) in r['ports']
               for r in session.snapshot['rows']), 'Only test owned live instances'
    executable = checked(Path(package['directory']) / 'bin/mysql.exe')
    env = dict(ENV, USERPROFILE=str(ROOT / 'runtime'), APPDATA=str(ROOT / 'runtime'),
               TEMP=str(ROOT / 'runtime/temp'), TMP=str(ROOT / 'runtime/temp'))
    result = subprocess.run([str(executable), '--no-defaults', '--protocol=TCP',
                             '--host=127.0.0.1', f'--port={db_port}', '--user=root',
                             '--batch', '--skip-column-names', '--default-character-set=utf8mb4'],
                            input=sql, text=True, encoding='utf-8', capture_output=True,
                            cwd=ROOT, env=env, timeout=30, creationflags=subprocess.CREATE_NO_WINDOW)
    assert result.returncode == 0, result.stderr
    return result.stdout.strip()


def redis(session, *args):
    assert any(r['id'] == 'redis' and r['running'] for r in session.snapshot['rows'])
    data = f'*{len(args)}\r\n' + ''.join(f'${len(a.encode())}\r\n{a}\r\n' for a in args)
    with socket.create_connection(('127.0.0.1', session.snapshot['config']['redis']['port']), timeout=5) as conn:
        conn.sendall(data.encode())
        return conn.recv(8192).decode()


def rename_environment(destination):
    global ROOT
    source = checked(ROOT)
    destination = checked(destination)
    assert not destination.exists()
    # Windows directory moves use one PowerShell process after absolute boundary/reparse checks.
    quote = lambda s: "'" + str(s).replace("'", "''") + "'"
    command = '$ErrorActionPreference = "Stop"; Move-Item -LiteralPath ' + quote(source) + ' -Destination ' + quote(destination)
    subprocess.run([r'C:\Program Files\PowerShell\7\pwsh.exe', '-NoProfile', '-Command', command],
                   check=True, cwd=WORKSPACE, env=ENV, creationflags=subprocess.CREATE_NO_WINDOW)
    ROOT = destination


def main():
    checked(ROOT).mkdir(parents=True)
    source = checked(WORKSPACE / 'bin')
    for directory, dirs, files in os.walk(source, followlinks=False):
        checked(directory)
        for name in dirs + files:
            checked(Path(directory) / name)
    shutil.copytree(source, ROOT / 'bin')
    session = Session()
    try:
        session.call('scan')
        config = copy.deepcopy(session.snapshot['config'])
        packages = {p['version']: p for p in session.snapshot['packages']}
        assert all(p['error'] is None for p in packages.values())
        config['http_port'] = port()
        used = {config['http_port']}
        def independent_port():
            while True:
                candidate = port()
                if candidate not in used:
                    used.add(candidate)
                    return candidate
        for php in config['php']:
            php.update(port=independent_port(), workers=1, usage='normal')
            php['directory_entry'].update(enabled=False, directory=str(ROOT / 'www'))
        config['redis'].update(port=independent_port(), usage='normal')
        config['mysql'] = [dict(id='db57', name='relocation', series='5.7',
            package=packages['5.7.44']['id'], port=independent_port(), usage='normal', reuse_data=False),
            dict(id='db80', name='relocation', series='8.0', package=packages['8.0.44']['id'],
                 port=independent_port(), usage='normal', reuse_data=False)]
        config['projects'] = []
        for index, version in enumerate(('7.4.33', '8.3.35', '8.5.11')):
            site = checked(ROOT / f'www/网站 {index}/public')
            site.mkdir(parents=True)
            (site / 'index.php').write_text('<?php echo PHP_VERSION;', encoding='utf-8')
            config['projects'].append(dict(id=f'site{index}', name=f'网站 {index}',
                directory=str(site.parent), document_root='public', php=packages[version]['id'],
                enabled=True, virtual_host=False, domain=f'site{index}.localhost'))
        session.call('apply', config=config)
        for php in config['php']:
            service_id = 'php:' + php['package']
            text = session.call('read_config', id=service_id)['text']
            text += '\n; relocation user setting\nmax_execution_time=47\n'
            session.call('save_config', id=service_id, text=text)
        session.call('start_all')
        session.call('verify')
        for db in config['mysql']:
            package = next(p for p in session.snapshot['packages'] if p['id'] == db['package'])
            mysql(session, package, db['port'], 'CREATE DATABASE relocate_test; CREATE TABLE relocate_test.keep_data (v INT); INSERT INTO relocate_test.keep_data VALUES (47);')
        assert redis(session, 'SET', 'relocation-check', '47').startswith('+OK')
        state = json.loads((ROOT / 'runtime/state/processes.json').read_text(encoding='utf-8'))
        assert all(not Path(r['identity']['executable']).is_absolute() and not Path(r['config']).is_absolute() for r in state['records'])
        assert all(not Path(p).is_absolute() for p in state['applied_files'])
        assert all(not Path(p['directory']).is_absolute() for p in state['applied']['projects'])
        passed('Apache, three PHP versions, MySQL 5.7/8.0 and Redis run with relative persisted records')
        # Simulate loss of the manager, retaining only services started by this test.
        session.close()
        session = Session()
        session.call('scan')
        assert len([r for r in session.snapshot['rows'] if r['running']]) == 7
        session.call('verify')
        passed('Relative runtime records recover owned services after manager restart')
        session.call('stop_all')
        session.close()
        session = None
        rename_environment(BASE / 'renamed environment')
        session = Session()
        session.call('scan')
        assert session.snapshot['applied_revision'] is None
        assert all(not Path(p['directory']).is_absolute() for p in session.snapshot['config']['projects'])
        for php in config['php']:
            text = session.call('read_config', id='php:' + php['package'])['text']
            assert 'max_execution_time=47' in text
            assert 'original environment' not in text
        session.call('start_all')
        session.call('verify')
        for db in config['mysql']:
            package = next(p for p in session.snapshot['packages'] if p['id'] == db['package'])
            assert mysql(session, package, db['port'], 'SELECT v FROM relocate_test.keep_data;') == '47'
        assert '47' in redis(session, 'GET', 'relocation-check')
        passed('Renamed environment with spaces restarts all services and retains MySQL/Redis data and PHP settings')
        session.call('stop_all')
        session.close()
        session = None
        files = [ROOT / 'config/paths.json', ROOT / 'config/app.json', *list((ROOT / 'config/services').rglob('*.ini')),
                 *list((ROOT / 'config/services').rglob('*.conf')), *list((ROOT / 'config/services').rglob('*.managed.json'))]
        before = {str(p): (p.read_bytes(), p.stat().st_mtime_ns) for p in files}
        session = Session()
        session.call('scan')
        session.close()
        session = None
        assert before == {str(p): (p.read_bytes(), p.stat().st_mtime_ns) for p in files}
        passed('Unchanged restart and component scan preserve configuration bytes and modification times')
        evidence = checked(WORKSPACE / 'testwork/evidence/relocation.json')
        evidence.parent.mkdir(exist_ok=True)
        evidence.write_text(json.dumps(dict(version='0.2.1', executable=str(EXE), root=str(ROOT),
            packages=[dict(version=p['version'], id=p['id']) for p in packages.values()],
            results=RESULTS), ensure_ascii=False, indent=2), encoding='utf-8')
    finally:
        if session is not None and session.proc.poll() is None:
            session.call('stop_all')
            session.close()


if __name__ == '__main__':
    main()
