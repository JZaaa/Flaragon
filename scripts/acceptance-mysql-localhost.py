"""Compare owned MySQL/PHP HTTP fixtures before and after dual-loopback support."""
import argparse
import copy
import importlib.util
import json
import os
from pathlib import Path
import shutil
import socket
import statistics
import subprocess
import time

spec = importlib.util.spec_from_file_location('web', Path(__file__).with_name('acceptance-web-entries.py'))
web = importlib.util.module_from_spec(spec)
spec.loader.exec_module(web)
web.ROOT = web.ROOT.with_name(web.ROOT.name.replace('web-entries-', 'mysql-localhost-'))
checked = web.checked
USED_PORTS = set()


def copy_tree(source, destination):
    source, destination = checked(source), checked(destination)
    for directory, dirs, files in os.walk(source, followlinks=False):
        checked(directory)
        for name in dirs + files:
            checked(Path(directory) / name)
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copytree(source, destination)


def free_port():
    # Hold both families while choosing a port; the manager repeats this before launch.
    for port in range(32000, 40000):
        if port in USED_PORTS:
            continue
        with socket.socket() as v4, socket.socket(socket.AF_INET6) as v6:
            try:
                v4.bind(('127.0.0.1', port))
                v6.bind(('::1', port))
            except OSError:
                continue
            USED_PORTS.add(port)
            return port
    raise RuntimeError('No free dual-loopback port')


def close(session):
    session.process.stdin.close()
    assert session.process.wait(timeout=20) == 0


def sql(session, package, db, statement):
    assert any(r['id'] == 'mysql:' + db['id'] and r['running'] for r in session.snapshot['rows'])
    exe = checked(Path(package['executable']).with_name('mysql.exe'))
    env = {'SystemRoot': os.environ.get('SystemRoot', r'C:\Windows'),
           'FLARAGON_DEV_ROOT': str(web.WORKSPACE),
           'TEMP': str(web.ROOT / 'runtime/temp'), 'TMP': str(web.ROOT / 'runtime/temp'),
           'MYSQL_HISTFILE': str(web.ROOT / 'runtime/temp/mysql-history')}
    result = subprocess.run([str(exe), '--no-defaults', '--protocol=TCP', '-h127.0.0.1',
                             '-P' + str(db['port']), '-uroot', '-N', '-B', '-e', statement],
                            cwd=web.ROOT, env=env, capture_output=True, text=True, timeout=30,
                            creationflags=subprocess.CREATE_NO_WINDOW)
    assert result.returncode == 0, result.stderr
    return result.stdout.strip()


def measure(session, http_port, hosts):
    results = []
    for host in hosts:
        samples = []
        for _ in range(3):
            start = time.perf_counter()
            status, _, body = session.get(http_port, '/db/?host=' + host)
            elapsed = (time.perf_counter() - start) * 1000
            assert status == 200, (status, body)
            result = json.loads(body)
            assert result['pdo_value'] == 'kept' and result['mysqli_value'] == 'kept', result
            samples.append(dict(http_ms=elapsed, **result))
        results.append(dict(host=host, samples=samples))
    return results


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--baseline', type=Path, required=True, help='Preserved pre-fix CLI inside the workspace')
    parser.add_argument('--release', action='store_true')
    args = parser.parse_args()
    current = checked(web.EXE)
    baseline = checked(args.baseline)
    checked(web.ROOT).mkdir(parents=True, exist_ok=False)
    for component, name in [('mysql', 'mysql-8.0.44'), ('mysql', 'mysql-5.7.44'),
                            ('php', 'php-8.3.35'), ('apache', 'apache-2.4.69')]:
        copy_tree(web.WORKSPACE / 'bin' / component / name, web.ROOT / 'bin' / component / name)
    evidence = dict(environment=str(web.ROOT), baseline=str(baseline),
                    baseline_mtime=baseline.stat().st_mtime, executable=str(current))
    session = None
    try:
        web.EXE = baseline
        session = web.Session()
        session.call('scan')
        packages = {p['id']: p for p in session.snapshot['packages']}
        assert all(p['error'] is None for p in packages.values()), packages
        evidence['components'] = [{k: p[k] for k in ('id', 'version', 'executable')} for p in packages.values()]
        config = copy.deepcopy(session.snapshot['config'])
        config.update(http_port=free_port(), http_address='127.0.0.1', mysql=[])
        for p in packages.values():
            if p['kind'] == 'mysql':
                series = '.'.join(p['version'].split('.')[:2])
                config['mysql'].append(dict(id='db-' + series.replace('.', '-'), package=p['id'],
                    series=series, name='localhost-check', port=free_port(), usage='normal', reuse_data=False))
        db = next(d for d in config['mysql'] if d['series'] == '8.0')
        old_db = next(d for d in config['mysql'] if d['series'] == '5.7')
        php = config['php'][0]
        php.update(port=free_port(), workers=1)
        config['projects'] = [dict(id='db', name='Localhost fixture', group='', domain='',
            directory='www/db', document_root='', php=php['package'], enabled=True, listed=True)]
        fixture = '''<?php
header('Content-Type: application/json');
$host = $_GET['host'] ?? 'localhost';
if (!in_array($host, ['localhost', '127.0.0.1', '::1'], true)) { http_response_code(400); exit; }
$start = hrtime(true);
$pdo = new PDO('mysql:host='.$host.';port=PORT;dbname=localhost_check;charset=utf8mb4', 'root', '', [PDO::ATTR_ERRMODE=>PDO::ERRMODE_EXCEPTION]);
$pdoMs = (hrtime(true)-$start)/1e6;
$value = $pdo->query('SELECT value FROM items WHERE id=1')->fetchColumn();
$start = hrtime(true);
$mysqli = new mysqli($host, 'root', '', 'localhost_check', PORT);
$mysqliMs = (hrtime(true)-$start)/1e6;
echo json_encode(['php'=>PHP_VERSION, 'pdo_ms'=>$pdoMs, 'mysqli_ms'=>$mysqliMs,
 'pdo_value'=>$value, 'mysqli_value'=>$mysqli->query('SELECT value FROM items WHERE id=1')->fetch_row()[0]]);
'''.replace('PORT', str(db['port']))
        web.write(web.ROOT / 'www/db/index.php', fixture)
        session.call('apply', config=config)
        php_id = 'php:' + php['package']
        text = session.call('read_config', id=php_id)['text']
        session.call('save_config', id=php_id, text=text + '\nextension=pdo_mysql\nextension=mysqli\n')
        db_id = 'mysql:' + db['id']
        native = session.call('read_config', id=db_id)['text']
        assert 'bind-address=127.0.0.1\n' in native and 'bind-address=127.0.0.1,::1' not in native
        session.call('save_config', id=db_id, text=native + '\n[mysqld]\nconnect_timeout=17\n')
        session.call('start', id=db_id)
        sql(session, packages[db['package']], db, 'CREATE DATABASE localhost_check; CREATE TABLE localhost_check.items (id INT PRIMARY KEY, value VARCHAR(20)); INSERT INTO localhost_check.items VALUES (1,\'kept\');')
        session.call('start', id='apache')
        evidence['before'] = measure(session, config['http_port'], ['127.0.0.1', 'localhost'])
        close(session)
        session = None
        print('PASS baseline measured; owned services retained for recovery', flush=True)

        web.EXE = current
        session = web.Session()
        session.call('scan')
        assert next(r for r in session.snapshot['rows'] if r['id'] == db_id)['state'] == '运行中'
        session.call('stop_all')
        # Conflict must be rejected before initializing fresh data or connecting to the occupant.
        config = copy.deepcopy(session.snapshot['config'])
        fresh = dict(db, id='ipv6-conflict', name='ipv6-conflict', port=free_port())
        config['mysql'].append(fresh)
        session.call('settings', config=config)
        with socket.socket(socket.AF_INET6) as occupied:
            occupied.bind(('::1', fresh['port']))
            occupied.listen()
            occupied.settimeout(0.2)
            result = session.call('start', expect=False, id='mysql:' + fresh['id'])
            assert '[::1]' in result['error'], result['error']
            assert not checked(web.ROOT / 'data/mysql/8.0/ipv6-conflict').exists()
            try:
                connection, _ = occupied.accept()
            except TimeoutError:
                pass
            else:
                connection.close()
                raise AssertionError('Connected to occupied IPv6 listener')
        config['mysql'].pop()
        session.call('settings', config=config)
        session.call('start', id=db_id)
        session.call('start', id='mysql:' + old_db['id'])
        assert sql(session, packages[old_db['package']], old_db, 'SELECT VERSION()').startswith('5.7.44')
        assert 'bind-address=127.0.0.1\n' in session.call('read_config', id='mysql:' + old_db['id'])['text']
        native = session.call('read_config', id=db_id)['text']
        assert 'bind-address=127.0.0.1,::1\n' in native and 'connect_timeout=17' in native
        assert sql(session, packages[db['package']], db, 'SELECT @@bind_address, @@connect_timeout') == '127.0.0.1,::1\t17'
        session.call('start', id='apache')
        evidence['after'] = measure(session, config['http_port'], ['127.0.0.1', 'localhost', '::1'])
        for result in evidence['after']:
            for driver in ['pdo_ms', 'mysqli_ms']:
                assert statistics.median(s[driver] for s in result['samples']) < 500, result
        before_local = next(r for r in evidence['before'] if r['host'] == 'localhost')
        after_local = next(r for r in evidence['after'] if r['host'] == 'localhost')
        for driver in ['pdo_ms', 'mysqli_ms']:
            assert statistics.median(s[driver] for s in after_local['samples']) < statistics.median(s[driver] for s in before_local['samples']) / 5
        close(session)
        session = web.Session()
        session.call('scan')
        assert all(r['state'] == '运行中' for r in session.snapshot['rows'] if r['id'] in [db_id, 'mysql:' + old_db['id']]), session.snapshot['rows']
        session.call('restart', id=db_id)
        evidence['after_restart'] = measure(session, config['http_port'], ['localhost'])
        session.call('stop_all')
        session.call('save_config', expect=False, id=db_id, text=native.replace('bind-address=127.0.0.1,::1', 'bind-address=*'))
        session.call('save_config', expect=False, id=db_id, text=native + '\n[mysqld]\nbind-address=*\n')
        for address, family in [('127.0.0.1', socket.AF_INET), ('::1', socket.AF_INET6)]:
            with socket.socket(family) as check:
                check.bind((address, db['port']))
        evidence['checks'] = ['IPv6 conflict rejected before initialization without connecting',
            'legacy managed configuration upgraded; custom options and data retained',
            'PDO and mysqli HTTP requests through localhost, IPv4, IPv6',
            'MySQL 5.7 IPv4 regression', 'running process recovery and normal restart',
            'wildcard override rejected', 'both listeners released after normal stop']
        evidence['passed'] = True
        print('PASS dual-loopback integration', flush=True)
    finally:
        if session is not None:
            try:
                session.call('stop_all')
            finally:
                close(session)
        web.write(web.ROOT / 'results.json', json.dumps(evidence, ensure_ascii=False, indent=2))
        print('EVIDENCE', web.ROOT / 'results.json', flush=True)


if __name__ == '__main__':
    main()
