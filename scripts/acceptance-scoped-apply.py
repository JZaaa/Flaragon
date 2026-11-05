"""Validate scoped saves and editable listeners using fresh workspace-owned services."""
import copy
import importlib.util
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import time

spec = importlib.util.spec_from_file_location('web_fixture', Path(__file__).with_name('acceptance-web-entries.py'))
web = importlib.util.module_from_spec(spec)
spec.loader.exec_module(web)
web.ROOT = web.ROOT.with_name(web.ROOT.name.replace('web-entries-', 'scoped-apply-'))
web.EXE = web.WORKSPACE / 'target/x86_64-pc-windows-msvc/release/flaragon-cli.exe'
ROOT = web.ROOT
if '--fixture' in sys.argv:
    ROOT = web.checked(Path(sys.argv[sys.argv.index('--fixture') + 1]))
    web.ROOT = ROOT
results = []
used_ports = set()


def passed(name):
    results.append(name)
    print('PASS', name, flush=True)


def port():
    while True:
        with socket.socket() as s:
            s.bind(('0.0.0.0', 0))
            n = s.getsockname()[1]
        if n not in used_ports:
            used_ports.add(n)
            return n


def identities():
    data = json.loads((ROOT / 'runtime/state/processes.json').read_text(encoding='utf-8'))
    return {r['id']: r['identity'] for r in data['records']}


def changed_only(before, after, changed):
    assert before.keys() == after.keys(), (before.keys(), after.keys())
    assert {k for k in before if before[k] != after[k]} == set(changed)


def redis(session, address, *args):
    assert next(r for r in session.snapshot['rows'] if r['id'] == 'redis')['running']
    wire = f'*{len(args)}\r\n' + ''.join(f'${len(a.encode())}\r\n{a}\r\n' for a in args)
    with socket.create_connection((address, session.snapshot['config']['redis']['port']), timeout=4) as stream:
        stream.sendall(wire.encode())
        return stream.recv(8192).decode()


def mysql(session, db, host, sql):
    assert next(r for r in session.snapshot['rows'] if r['id'] == 'mysql:' + db['id'])['running']
    package = next(p for p in session.snapshot['packages'] if p['id'] == db['package'])
    exe = web.checked(Path(package['executable']).with_name('mysql.exe'))
    env = dict(SystemRoot=os.environ.get('SystemRoot', r'C:\Windows'), FLARAGON_DEV_ROOT=str(web.WORKSPACE),
               TEMP=str(ROOT / 'runtime/temp'), TMP=str(ROOT / 'runtime/temp'))
    result = subprocess.run([str(exe), '--no-defaults', '--protocol=TCP', '-h' + host,
                             '-P' + str(db['port']), '-uroot', '-N', '-B', '-e', sql],
                            cwd=ROOT, env=env, capture_output=True, text=True, timeout=30,
                            creationflags=subprocess.CREATE_NO_WINDOW)
    assert result.returncode == 0, result.stderr
    return result.stdout.strip()


def main():
    web.checked(ROOT).mkdir(parents=True, exist_ok=True)
    for kind, name in [('apache', 'apache-2.4.69'), ('php', 'php-8.3.35'), ('php', 'php-7.4.33'),
                       ('mysql', 'mysql-8.0.44'), ('mysql', 'mysql-5.7.44'), ('redis', 'redis-7.4.3')]:
        source = web.checked(web.WORKSPACE / 'bin' / kind / name)
        for directory, folders, files in os.walk(source, followlinks=False):
            web.checked(directory)
            for child in folders + files:
                web.checked(Path(directory) / child)
        destination = web.checked(ROOT / 'bin' / kind / name)
        if destination.exists():
            continue
        shutil.copytree(source, destination,
                        ignore=shutil.ignore_patterns('instances', '*.managed.json', '*.backup', '*.before-edit',
                                                     'php.ini', 'redis.conf'))
    session = web.Session()
    try:
        session.call('scan')
        config = copy.deepcopy(session.snapshot['config'])
        config.update(http_port=port(), http_address='127.0.0.1', mysql=[])
        for php in config['php']:
            php.update(port=port(), workers=1)
            php['directory_entry']['enabled'] = False
        for p in session.snapshot['packages']:
            if p['kind'] == 'mysql':
                series = '.'.join(p['version'].split('.')[:2])
                config['mysql'].append(dict(id='db-' + series.replace('.', '-'), name='scoped', series=series,
                    package=p['id'], port=port(), usage='normal', reuse_data=False))
        config['redis']['port'] = port()
        php = config['php'][0]['package']
        other_php = config['php'][1]['package']
        config['projects'] = [dict(id=name, name=name, directory='www/' + name, document_root='',
            php=php, enabled=True, listed=True, lan_access=True) for name in ['first', 'second']]
        for name in ['first', 'second', 'third']:
            web.write(ROOT / ('www/' + name + '/index.php'), '<?php echo PHP_VERSION, "|", ini_get("precision");')
        session.call('apply', config=config)
        assert not identities()
        session.call('start_all')
        before = identities()
        c = copy.deepcopy(session.snapshot['config'])
        c['projects'].append(dict(id='third', name='third', directory='www/third', document_root='',
            php=other_php, enabled=True, listed=True, lan_access=True))
        session.call('apply', config=c)
        assert identities() == before
        assert session.get(c['http_port'], '/third/')[0] == 200
        portal = session.get(c['http_port'])[2]
        assert 'PHP 8.3.35' in portal and 'PHP 7.4.33' in portal
        passed('Project save completes once using Apache reload; all process identities preserved; portal shows PHP versions')
        native = session.call('read_config', id='apache')['text']
        session.call('save_config', id='apache', text=native.replace('# BEGIN FLARAGON PROJECTS', 'KeepAliveTimeout 7\n# BEGIN FLARAGON PROJECTS'))
        assert identities() == before
        passed('Apache native edit reloads without restarting PHP or databases')
        native = session.call('read_config', id='php:' + php)['text']
        session.call('save_config', id='php:' + php, text=native + '\nprecision=11\n')
        after = identities()
        changed_only(before, after, ['php:' + php])
        assert session.get(c['http_port'], '/first/')[2].endswith('|11')
        before = after
        passed('PHP ini edit restarts only its pool and HTTP observes the value')
        session.call('switch_php', id=php, target=other_php)
        assert identities() == before
        assert all(p['php'] == other_php for p in session.snapshot['config']['projects'])
        passed('Bulk PHP switch updates every referenced project while preserving all existing processes')
        config_before = copy.deepcopy(session.snapshot['config'])
        session.call('switch_php', expect=False, id=other_php, target='missing-target')
        assert session.snapshot['config'] == config_before and identities() == before
        no_php = copy.deepcopy(config_before)
        no_php['projects'][0]['php'] = None
        session.call('apply', expect=False, config=no_php)
        assert session.snapshot['config'] == config_before and identities() == before
        passed('Missing bulk target and implicit project PHP are rejected before changing services')
        native = session.call('read_config', id='apache')['text']
        session.call('save_config', expect=False, id='apache', text=native.replace('# BEGIN FLARAGON PROJECTS', 'InvalidFlaragonDirective on\n# BEGIN FLARAGON PROJECTS'))
        assert identities() == before
        assert session.get(c['http_port'], '/first/')[0] == 200
        passed('Invalid Apache save keeps live services and restores the file')
        for db in session.snapshot['config']['mysql']:
            db_id = 'mysql:' + db['id']
            native = session.call('read_config', id=db_id)['text']
            assert 'bind-address=0.0.0.0' in native
            mysql(session, db, '127.0.0.1', 'CREATE DATABASE scoped_check; CREATE TABLE scoped_check.items (id INT); INSERT INTO scoped_check.items VALUES (7)')
            session.call('save_config', id=db_id, text=native.replace('bind-address=0.0.0.0', 'bind-address=127.0.0.2'))
            after = identities()
            changed_only(before, after, [db_id])
            assert mysql(session, db, '127.0.0.2', 'SELECT @@bind_address, id FROM scoped_check.items') == '127.0.0.2\t7'
            before = after
        passed('MySQL 5.7 and 8.0 bind edits restart only the selected instance; data and custom address verified')
        native = session.call('read_config', id='redis')['text']
        session.call('save_config', id='redis', text=native + '\nbind 127.0.0.2\n')
        after = identities()
        changed_only(before, after, ['redis'])
        assert redis(session, '127.0.0.2', 'PING') == '+PONG\r\n'
        assert redis(session, '127.0.0.2', 'SET', 'scoped-check', 'kept') == '+OK\r\n'
        before = after
        native = session.call('read_config', id='redis')['text']
        session.call('save_config', id='redis', text=native + '\nbind 0.0.0.0\n')
        after = identities()
        changed_only(before, after, ['redis'])
        assert 'kept' in redis(session, '127.0.0.1', 'GET', 'scoped-check')
        passed('Redis specific-address and wildcard binds retain data and restart only Redis')
        before = identities()
        native = session.call('read_config', id='redis')['text']
        session.call('save_config', expect=False, id='redis', text=native + '\nflaragon-invalid-option yes\n')
        changed_only(before, identities(), ['redis'])
        assert 'kept' in redis(session, '127.0.0.1', 'GET', 'scoped-check')
        assert session.call('read_config', id='redis')['text'] == native
        passed('Redis startup failure restores its previous configuration and data without touching other services')
        # The manager must keep a stopped instance stopped when saving its config.
        session.call('stop', id='redis')
        before = identities()
        native = session.call('read_config', id='redis')['text']
        session.call('save_config', id='redis', text=native + '\ntimeout 20\n')
        assert identities() == before and 'redis' not in identities()
        passed('Saving a stopped instance never starts it or restarts unrelated services')
        session.call('stop_all')
        missing = copy.deepcopy(session.snapshot['config'])
        missing['php'].append(dict(package='missing-php', port=port(), workers=1, usage='normal'))
        missing['projects'][0]['php'] = 'missing-php'
        session.call('apply', config=missing)
        session.call('switch_php', id='missing-php', target=other_php, remove_missing=True)
        assert all(p['package'] != 'missing-php' for p in session.snapshot['config']['php'])
        assert session.snapshot['config']['projects'][0]['php'] == other_php
        passed('Missing PHP removal offers a single bulk replacement operation preserving project files')
    finally:
        session.call('stop_all')
        session.process.stdin.close()
        assert session.process.wait(timeout=20) == 0
        web.write(ROOT / 'results.json', json.dumps(dict(environment=str(ROOT), executable=str(web.EXE),
            executable_mtime=web.EXE.stat().st_mtime, components=[{k:p[k] for k in ['id','version']} for p in session.snapshot['packages']],
            passed=results), ensure_ascii=False, indent=2))
        print('EVIDENCE', ROOT / 'results.json', flush=True)


if __name__ == '__main__':
    main()
