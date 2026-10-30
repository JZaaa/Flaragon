"""Exercise package-owned native configuration in a fresh project-local environment.

Run with scripts/env.ps1 loaded. The first run creates a fixture and stops every
service. --resume verifies a fixture after an explicitly checked directory rename.
--replacement verifies a fresh PHP package placed at the same package path.
"""
import argparse
import json
import os
from pathlib import Path
import shutil
import socket
import stat
import subprocess
import time
from http.client import HTTPConnection

WORKSPACE = Path(__file__).resolve().parent.parent
EXE = WORKSPACE / 'target/x86_64-pc-windows-msvc/release/flaragon-cli.exe'
SOURCES = {
    'php': ['php-7.4.33', 'php-8.3.35', 'php-8.5.11'],
    'mysql': ['mysql-5.7.44', 'mysql-8.0.44'],
    'redis': ['redis-7.4.3'],
    'apache': ['apache-2.4.69'],
}


def checked(path):
    path = Path(os.path.abspath(path))
    assert path.is_relative_to(WORKSPACE), path
    for item in [path, *path.parents]:
        if item == WORKSPACE.parent:
            break
        if item.exists():
            assert not (item.lstat().st_file_attributes & stat.FILE_ATTRIBUTE_REPARSE_POINT), item
    return path


def write(path, text):
    path = checked(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding='utf-8')


def copy_package(source, target):
    source, target = checked(source), checked(target)
    assert not target.exists(), target
    for directory, folders, files in os.walk(source, followlinks=False):
        checked(directory)
        for name in folders + files:
            checked(Path(directory) / name)
    shutil.copytree(source, target)


def environment(root):
    system = os.environ.get('SystemRoot', r'C:\Windows')
    return dict(SystemRoot=system, WINDIR=system,
                PATH=str(Path(system) / 'System32') + ';' + system,
                TEMP=str(root / 'runtime/temp'), TMP=str(root / 'runtime/temp'),
                USERPROFILE=str(root / 'runtime'), APPDATA=str(root / 'runtime'),
                LOCALAPPDATA=str(root / 'runtime'),
                COMPOSER_HOME=str(root / 'runtime/composer'),
                COMPOSER_CACHE_DIR=str(root / 'runtime/composer-cache'),
                COMPOSER_DISABLE_NETWORK='1',
                FLARAGON_DEV_ROOT=str(WORKSPACE))


def run(root, argv):
    checked(argv[0])
    result = subprocess.run([str(arg) for arg in argv], cwd=checked(root / 'www'),
                            env=environment(root), capture_output=True, text=True,
                            encoding='utf-8', timeout=30, creationflags=subprocess.CREATE_NO_WINDOW)
    assert result.returncode == 0, result.stderr
    return result.stdout.strip()


class Session:
    def __init__(self, root):
        self.root = checked(root)
        self.proc = subprocess.Popen([str(EXE), str(root)], cwd=root, env=environment(root),
                                     stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                     text=True, encoding='utf-8', creationflags=subprocess.CREATE_NO_WINDOW)
        self.snapshot = None

    def call(self, action, expect=True, **kwargs):
        self.proc.stdin.write(json.dumps(dict(action=action, **kwargs), ensure_ascii=False) + '\n')
        self.proc.stdin.flush()
        response = json.loads(self.proc.stdout.readline())
        self.snapshot = response['snapshot']
        assert response['ok'] == expect, response.get('error', action)
        return response

    def close(self):
        self.call('stop_all')
        self.proc.stdin.close()
        assert self.proc.wait(timeout=20) == 0


def mysql(session, package, port, sql):
    # Called only after the manager has confirmed this fixture's process ownership.
    executable = Path(package['directory']) / 'bin/mysql.exe'
    return run(session.root, [executable, '--no-defaults', '--protocol=TCP', '--host=127.0.0.1',
                             f'--port={port}', '--user=root', '--batch', '--skip-column-names', '-e', sql])


def redis(session, *args):
    port = session.snapshot['config']['redis']['port']
    assert next(row for row in session.snapshot['rows'] if row['id'] == 'redis')['running']
    wire = f'*{len(args)}\r\n' + ''.join(f'${len(a.encode())}\r\n{a}\r\n' for a in args)
    with socket.create_connection(('127.0.0.1', port), timeout=5) as connection:
        connection.sendall(wire.encode())
        return connection.recv(8192).decode()


def http(session, project):
    config = session.snapshot['config']
    assert next(row for row in session.snapshot['rows'] if row['id'] == 'apache')['running']
    connection = HTTPConnection('127.0.0.1', config['http_port'], timeout=10)
    connection.request('GET', '/' + project['id'] + '/')
    response = connection.getresponse()
    body = response.read().decode()
    connection.close()
    assert response.status == 200, body
    return json.loads(body)


def native_files(root):
    paths = [root / 'config/app.json', root / 'config/paths.json']
    for directory, _, files in os.walk(root / 'bin', followlinks=False):
        checked(directory)
        for name in files:
            if name in ['php.ini', 'redis.conf', 'my.ini'] or name.endswith('.managed.json'):
                paths.append(checked(Path(directory) / name))
    return [p for p in paths if p.is_file()]


def exercise(root, resume=False, replacement=False):
    session = Session(root)
    passed = []
    def record(name):
        passed.append(name)
        print('PASS', name, flush=True)
    try:
        session.call('scan')
        config = session.snapshot['config']
        packages = {p['id']: p for p in session.snapshot['packages']}
        assert all(p['error'] is None for p in packages.values())
        if replacement:
            spec = next(s for s in config['php'] if packages[s['package']]['version'].startswith('8.5.'))
            package = packages[spec['package']]
            text = session.call('read_config', id='php:' + spec['package'])['text']
            active = '\n'.join(line for line in text.splitlines() if not line.lstrip().startswith((';', '#')))
            assert 'extension=curl' not in active and 'missing_old_extension' not in text
            actual = json.loads(run(root, [package['executable'], '-r',
                "echo json_encode(['ini'=>php_ini_loaded_file(),'precision'=>ini_get('precision'),'curl'=>extension_loaded('curl')]);"]))
            assert actual['precision'] == '14' and not actual['curl'], actual
            session.call('start', id='php:' + spec['package'])
            record('Same-name fresh PHP package uses its own configuration and no retired extension declarations')
        else:
            if not resume:
                config.update(http_port=28480, projects=[])
                for index, spec in enumerate(config['php']):
                    spec.update(port=29400 + index * 8, workers=1)
                    project = dict(id='site' + str(index), name='中文显示名称 ' + str(index), directory='www/site space ' + str(index),
                                   document_root='', php=spec['package'], enabled=True)
                    config['projects'].append(project)
                    write(root / project['directory'] / 'index.php', "<?php header('Content-Type: application/json'); echo json_encode(['ini'=>php_ini_loaded_file(),'precision'=>ini_get('precision'),'curl'=>extension_loaded('curl')]);")
                    write(Path(packages[spec['package']]['directory']) / 'php.ini', '; package custom configuration\n[PHP]\nprecision=12\nextension=curl\nextension=openssl\n')
                    write(root / 'config/services/php' / spec['package'] / 'php.ini', 'extension=missing_old_extension\n')
                    write(root / 'config/overrides/php' / (spec['package'] + '.ini'), 'extension=missing_old_extension\n')
                config['mysql'] = []
                for index, version in enumerate(['5.7.', '5.7.', '8.0.']):
                    package = next(p for p in packages.values() if p['kind'] == 'mysql' and p['version'].startswith(version))
                    config['mysql'].append(dict(id='package' + str(index), name='package' + str(index),
                        series='.'.join(package['version'].split('.')[:2]), package=package['id'],
                        port=23407 + index, usage='normal', reuse_data=False))
                config['redis'].update(port=26479)
                session.call('apply', config=config)
                for index, spec in enumerate(config['mysql']):
                    native = session.call('read_config', id='mysql:' + spec['id'])
                    session.call('save_config', id='mysql:' + spec['id'], text=native['text'] + f'\n[mysqld]\nconnect_timeout={17 + index}\n')
                native = session.call('read_config', id='redis')
                session.call('save_config', id='redis', text=native['text'] + '\nhz 13\nappendonly yes\n')
            session.call('start_all')
            session.call('verify')
            state = json.loads((root / 'runtime/state/processes.json').read_text(encoding='utf-8'))
            for process in state['records']:
                native = session.call('read_config', id=process['id'])
                path = Path(native['path'])
                assert path == root / process['config']
                assert path.is_relative_to(root / ('config/services/apache' if process['id'] == 'apache' else 'bin'))
            for project in config['projects']:
                package = packages[project['php']]
                cli = json.loads(run(root, [package['executable'], '-r', "echo json_encode(['ini'=>php_ini_loaded_file(),'precision'=>ini_get('precision'),'curl'=>extension_loaded('curl')]);"]))
                web = http(session, project)
                assert cli == web, (cli, web)
                assert cli['precision'] == '12' and cli['curl'], cli
            record('PHP 7.4/8.3/8.5 CLI without -c and Apache load the same package php.ini and curl extension')
            composer = checked(WORKSPACE / 'testwork/cake-framework/composer.phar')
            if composer.is_file():
                write(root / 'www/composer.json', json.dumps({'scripts': {'probe': '@php composer-probe.php'}}))
                write(root / 'www/composer-probe.php', "<?php if(realpath(php_ini_loaded_file())!==realpath(dirname(PHP_BINARY).'/php.ini') || !extension_loaded('curl') || ini_get('precision')!=='12') exit(1); echo 'PACKAGE_INI_OK';")
                for project in config['projects']:
                    output = run(root, [packages[project['php']]['executable'], composer, '--no-interaction', '--no-plugins', 'run-script', 'probe'])
                    assert 'PACKAGE_INI_OK' in output, output
                record('Composer and its @php subprocess load the package ini without PHPRC or -c')
            for index, spec in enumerate(config['mysql']):
                package = packages[spec['package']]
                assert mysql(session, package, spec['port'], 'SELECT @@connect_timeout;') == str(17 + index)
                if not resume:
                    mysql(session, package, spec['port'], f'CREATE DATABASE package_test; CREATE TABLE package_test.marker(v INT); INSERT INTO package_test.marker VALUES({index});')
                assert mysql(session, package, spec['port'], 'SELECT v FROM package_test.marker;') == str(index)
            record('MySQL 5.7/8.0 and two same-package instances keep independent configuration and test data')
            if not resume:
                assert redis(session, 'SET', 'package-config-test', 'preserved').startswith('+OK')
            assert 'preserved' in redis(session, 'GET', 'package-config-test')
            assert '13' in redis(session, 'CONFIG', 'GET', 'hz')
            record('Redis loads package redis.conf and retains persisted test data')
            if not resume:
                spec = config['php'][-1]
                native = session.call('read_config', id='php:' + spec['package'])
                session.call('save_config', expect=False, id='php:' + spec['package'], text=native['text'] + '\nextension=missing_test_extension\n')
                assert session.call('read_config', id='php:' + spec['package'])['text'] == native['text']
                session.call('verify')
                record('Missing extension is rejected and the previous package configuration remains usable')
        session.close()
        session = None
        before = {p: (p.read_bytes(), p.stat().st_mtime_ns) for p in native_files(root)}
        session = Session(root)
        session.call('scan')
        assert before == {p: (p.read_bytes(), p.stat().st_mtime_ns) for p in before}
        record('Unchanged manager startup preserves native file bytes and modification times')
    finally:
        if session is not None:
            session.close()
        write(root / ('acceptance-replacement.json' if replacement else 'acceptance-moved.json' if resume else 'acceptance.json'),
              json.dumps(dict(executable=str(EXE), build_modified=EXE.stat().st_mtime_ns,
                              sources=SOURCES, passed=passed), ensure_ascii=False, indent=2))


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--resume', type=Path)
    parser.add_argument('--replacement', type=Path)
    args = parser.parse_args()
    assert Path(os.environ['FLARAGON_DEV_ROOT']).resolve() == WORKSPACE
    if args.resume or args.replacement:
        exercise(checked(args.resume or args.replacement), resume=bool(args.resume), replacement=bool(args.replacement))
    else:
        root = checked(WORKSPACE / 'testwork' / ('package-config-' + str(time.time_ns())))
        root.mkdir()
        for kind, names in SOURCES.items():
            for name in names:
                copy_package(WORKSPACE / 'bin' / kind / name, root / 'bin' / kind / name)
        (root / 'www').mkdir()
        (root / 'runtime/temp').mkdir(parents=True)
        write(WORKSPACE / 'testwork/package-config-latest.txt', str(root))
        exercise(root)
