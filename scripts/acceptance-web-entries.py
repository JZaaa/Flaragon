"""Validate Apache directory and managed-path entries in a fresh project-local environment."""
import concurrent.futures
import copy
import http.client
import json
import os
from pathlib import Path
import queue
import shutil
import socket
import stat
import subprocess
import sys
import threading
import uuid

WORKSPACE = Path(__file__).resolve().parent.parent
ROOT = WORKSPACE / 'testwork' / ('web-entries-' + uuid.uuid4().hex[:12])
EXE = WORKSPACE / ('target/x86_64-pc-windows-msvc/' + ('release' if '--release' in sys.argv else 'debug') + '/flaragon-cli.exe')
RESULTS = []
EXTERNAL_SITES = '--external-sites' in sys.argv
SITE_ROOT = ROOT.with_name(ROOT.name + '-sites') if EXTERNAL_SITES else ROOT


def checked(path):
    path = Path(os.path.abspath(path))
    if not path.is_relative_to(WORKSPACE):
        raise RuntimeError('Path outside workspace')
    for ancestor in (path, *path.parents):
        if ancestor.exists() and ancestor.lstat().st_file_attributes & stat.FILE_ATTRIBUTE_REPARSE_POINT:
            raise RuntimeError('Reparse point rejected')
    return path


def write(path, content):
    path = checked(path)
    checked(path.parent).mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding='utf-8')


def passed(name):
    RESULTS.append({'name': name, 'passed': True})
    print('PASS', name, flush=True)


class Session:
    def __init__(self):
        env = {'SystemRoot': os.environ.get('SystemRoot', r'C:\Windows'),
               'FLARAGON_DEV_ROOT': str(WORKSPACE),
               'TEMP': str(WORKSPACE / '.cache/tmp'), 'TMP': str(WORKSPACE / '.cache/tmp')}
        self.process = subprocess.Popen([str(checked(EXE)), str(ROOT)], cwd=ROOT, env=env,
                                        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                        text=True, encoding='utf-8', creationflags=subprocess.CREATE_NO_WINDOW)
        self.responses = queue.Queue()
        threading.Thread(target=lambda: [self.responses.put(line) for line in self.process.stdout], daemon=True).start()
        self.snapshot = None

    def call(self, action, expect=True, **kwargs):
        self.process.stdin.write(json.dumps(dict(action=action, **kwargs)) + '\n')
        self.process.stdin.flush()
        result = json.loads(self.responses.get(timeout=100))
        self.snapshot = result['snapshot']
        assert result['ok'] == expect, result.get('error', action)
        return result

    def get(self, port, path='/', host='localhost', method='GET', body=None, headers=None):
        # Only connect after the manager has verified its own process owns this port.
        assert any(r['id'] == 'apache' and r['running'] and str(port) in r['ports'].split(', ') for r in self.snapshot['rows'])
        conn = http.client.HTTPConnection('127.0.0.1', port, timeout=8)
        h = dict(headers or {}); h['Host'] = host
        conn.request(method, path, body=body, headers=h)
        response = conn.getresponse()
        result = (response.status, dict(response.getheaders()), response.read().decode('utf-8', errors='replace'))
        conn.close()
        return result


def main():
    checked(ROOT).mkdir(parents=True)
    for component in ['apache', 'php']:
        source = checked(WORKSPACE / 'bin' / component)
        for directory, folders, files in os.walk(source, followlinks=False):
            checked(directory)
            for name in folders + files: checked(Path(directory) / name)
        shutil.copytree(source, ROOT / 'bin' / component)
    session = Session()
    try:
        session.call('scan')
        config = copy.deepcopy(session.snapshot['config'])
        packages = {p['id']: p for p in session.snapshot['packages']}
        assert all(p['error'] is None for p in packages.values())
        config.update(http_port=28180, http_address='127.0.0.1', apache_usage='common')
        for index, php in enumerate(config['php']):
            php.update(port=29200 + index * 8, usage='normal', directory_entry=dict(enabled=True, port=28174 + index,
                       directory=str(SITE_ROOT / 'www/directory test') if EXTERNAL_SITES else 'www/directory test', address='127.0.0.1'))
        config['projects'] = []
        write(SITE_ROOT / 'www/directory test/free/index.php', '<?php header("Content-Type: application/json"); echo json_encode(["version"=>PHP_VERSION,"uri"=>$_SERVER["REQUEST_URI"]]);')
        session.call('apply', config=config)
        session.call('start', id='apache')
        for php in config['php']:
            status, _, body = session.get(php['directory_entry']['port'], '/free/')
            assert status == 200 and json.loads(body)['version'] == packages[php['package']]['version'], body
        passed('Directory entries run all three PHP versions without managed projects')
        rejected = session.call('stop', expect=False, id='php:' + config['php'][0]['package'])
        assert 'Apache' in rejected['error']
        changed = copy.deepcopy(session.snapshot['config']); changed['php'][0]['directory_entry']['port'] += 10
        session.call('settings', expect=False, config=changed)
        passed('Directory dependencies prevent stopping PHP and editing active entries')
        session.call('stop_all')
        config = copy.deepcopy(session.snapshot['config'])
        script = '''<?php
ini_set('session.cookie_path', '/' . explode('/', trim($_SERVER['REQUEST_URI'], '/'))[0] . '/');
session_start(); $_SESSION['visits'] = ($_SESSION['visits'] ?? 0) + 1;
header('Content-Type: application/json');
echo json_encode(['version'=>PHP_VERSION,'uri'=>$_SERVER['REQUEST_URI'],'script'=>$_SERVER['SCRIPT_NAME'], 'root'=>$_SERVER['DOCUMENT_ROOT'], 'visits'=>$_SESSION['visits'],'method'=>$_SERVER['REQUEST_METHOD'],'data'=>file_get_contents('php://input')]);
'''
        for i in range(24):
            folder = 'www/directory test/site ' + str(i)
            if EXTERNAL_SITES:
                folder = str(SITE_ROOT / folder)
            write(ROOT / folder / 'webroot/index.php', script)
            write(ROOT / folder / 'webroot/asset.txt', 'asset-' + str(i))
            write(ROOT / folder / 'webroot/.htaccess', 'RewriteEngine On\nRewriteCond %{REQUEST_FILENAME} !-f\nRewriteRule ^ index.php [L]\n')
            write(ROOT / folder / '.htaccess', 'RewriteEngine On\nRewriteRule ^$ webroot/ [L]\nRewriteRule (.*) webroot/$1 [L]\n')
            write(ROOT / folder / 'config/private.txt', 'not public')
            config['projects'].append(dict(id='site-' + str(i), name='中文 项目 ' + str(i), group='业务' if i % 2 else '工具',
                directory=folder, document_root='webroot', php=config['php'][i % len(config['php'])]['package'],
                enabled=True, listed=i != 23, lan_access=True, domain='site-' + str(i) + '.localhost'))
        session.call('apply', config=config)
        session.call('start', id='apache')
        for i in range(24):
            status, _, body = session.get(28180, f'/site-{i}/users/view/42?mode=test')
            assert status == 200, (i, status, body)
            data = json.loads(body)
            assert data['version'] == packages[config['projects'][i]['php']]['version'], data
            assert data['uri'] == f'/site-{i}/users/view/42?mode=test', data
            assert data['script'] == f'/site-{i}/index.php', data
            assert session.get(28180, f'/site-{i}/asset.txt')[2] == 'asset-' + str(i)
        passed('24 managed paths select PHP versions and preserve rewrites, query strings and static assets')
        for php in config['php']:
            assert json.loads(session.get(php['directory_entry']['port'], '/free/')[2])['version'] == packages[php['package']]['version']
        passed('Directory and managed entries coexist in one Apache process')
        status, headers, body = session.get(28180, '/site-0')
        assert status == 308 and headers['Location'].endswith('/site-0/')
        assert session.get(28180, '/missing/')[0] == 404
        assert session.get(28180, '/site-0/config/private.txt')[2] != 'not public'
        page = session.get(28180)[2]
        assert 'href="/site-0/"' in page and 'href="/site-23/"' not in page
        assert str(ROOT) not in page and 'data-group="业务"' in page
        assert 'href="/site-0/"' in session.get(28180, host='site-0.localhost')[2]
        passed('Static navigation, hidden listings, unknown paths and virtual hosts disabled by default')
        status, headers, body = session.get(28180, '/site-0/login')
        cookie = headers['Set-Cookie'].split(';')[0]
        assert 'path=/site-0/' in headers['Set-Cookie']
        assert json.loads(session.get(28180, '/site-0/login', headers={'Cookie': cookie})[2])['visits'] == 2
        assert json.loads(session.get(28180, '/site-1/login')[2])['visits'] == 1
        data = json.loads(session.get(28180, '/site-0/upload', method='POST', body='body-test', headers={'Content-Type': 'text/plain'})[2])
        assert data['data'] == 'body-test' and data['method'] == 'POST'
        with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
            assert all(pool.map(lambda i: session.get(28180, f'/site-{i % 24}/users')[0] == 200, range(48)))
        passed('Session path fixture, POST bodies and 48 concurrent managed requests')
        before = {r['id']: r['detail'] for r in session.snapshot['rows'] if r['running']}
        session.call('reload', id='apache')
        after = {r['id']: r['detail'] for r in session.snapshot['rows'] if r['running']}
        assert before == after
        passed('Apache reload verifies all entries while preserving process identities')
        config = copy.deepcopy(session.snapshot['config'])
        config['projects'][0]['virtual_host'] = True
        config['projects'][0]['name'] = '改名保持路径'
        session.call('apply', config=config)
        assert json.loads(session.get(28180, '/', host='site-0.localhost')[2])['version'] == packages[config['projects'][0]['php']]['version']
        assert session.get(28180, '/site-0/')[0] == 200
        passed('Explicit virtual-host opt-in and display-name changes preserve managed URLs')
        config = copy.deepcopy(session.snapshot['config'])
        config['projects'][0]['virtual_host'] = False
        config['projects'][0]['php'] = config['php'][1]['package']
        duplicate = copy.deepcopy(config['projects'][0])
        duplicate.update(id='same-root', php=config['php'][2]['package'])
        config['projects'].append(duplicate)
        write(ROOT / config['projects'][0]['directory'] / 'webroot/UPPER.PHP', '<?php echo PHP_VERSION;')
        session.call('apply', config=config)
        assert 'href="/site-0/"' in session.get(28180, host='site-0.localhost')[2]
        assert session.get(28180, '/site-0/UPPER.PHP')[2] == packages[config['php'][1]['package']]['version']
        assert session.get(28180, '/same-root/UPPER.PHP')[2] == packages[config['php'][2]['package']]['version']
        assert session.get(config['php'][0]['directory_entry']['port'], '/%E7%AB%99%E7%82%B9%200/webroot/UPPER.PHP')[2] == packages[config['php'][0]['package']]['version']
        passed('Version changes preserve paths; shared roots select PHP by URL; uppercase PHP executes; virtual hosts can be disabled')
        config = copy.deepcopy(session.snapshot['config']); config['projects'][0]['document_root'] = 'missing'
        session.call('apply', expect=False, config=config)
        assert session.get(28180, '/site-0/')[0] == 200
        passed('Invalid document root is rejected without losing the running configuration')
        if EXTERNAL_SITES:
            config = copy.deepcopy(session.snapshot['config'])
            config['projects'][0]['directory'] = str(WORKSPACE.with_name('flaragon-denied'))
            result = session.call('apply', expect=False, config=config)
            assert '开发隔离范围' in result['error'], result['error']
            assert session.get(28180, '/site-0/')[0] == 200
            passed('Development boundary rejects external workspace paths before filesystem access')
        session.call('stop_all')
        config = copy.deepcopy(session.snapshot['config']); config['projects'].clear()
        config['php'][0]['usage'] = 'disabled'
        session.call('apply', config=config)
        session.call('start_common', expect=False)
        assert not any(r['running'] for r in session.snapshot['rows'])
        passed('Disabled directory PHP dependency rejects startup before launching the batch')
        config = copy.deepcopy(session.snapshot['config']); config['php'][0]['usage'] = 'normal'
        session.call('apply', config=config)
        with socket.socket() as occupied:
            occupied.bind(('127.0.0.1', config['php'][0]['directory_entry']['port']))
            occupied.listen()
            session.call('start_common', expect=False)
            assert not any(r['running'] for r in session.snapshot['rows'])
        passed('Directory HTTP port conflicts roll back only the started batch')
    finally:
        try:
            session.call('stop_all')
        finally:
            session.process.stdin.close()
            session.process.wait(timeout=15)
        evidence = dict(environment=str(ROOT), executable=str(EXE), packages=[{'id':p['id'], 'version':p['version']} for p in session.snapshot['packages']], results=RESULTS)
        evidence['site_root'] = str(SITE_ROOT)
        write(WORKSPACE / ('testwork/evidence/external-site-entries.json' if EXTERNAL_SITES else 'testwork/evidence/web-entries.json'), json.dumps(evidence, ensure_ascii=False, indent=2))
        print('ENVIRONMENT', ROOT, flush=True)


if __name__ == '__main__':
    main()
