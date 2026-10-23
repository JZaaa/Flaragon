"""Exercise service usage and MySQL data isolation in a new workspace-local environment."""
import copy
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import uuid

WORKSPACE = Path(__file__).resolve().parent.parent
ROOT = WORKSPACE / 'testwork' / ('service-settings-' + uuid.uuid4().hex[:12])
EXE = WORKSPACE / 'target/x86_64-pc-windows-msvc/debug/flaragon-cli.exe'
RESULTS = []


def checked(path):
    path = Path(os.path.abspath(path))
    if not path.is_relative_to(WORKSPACE):
        raise RuntimeError('Path outside workspace')
    for ancestor in (path, *path.parents):
        if ancestor.exists() and ancestor.lstat().st_file_attributes & stat.FILE_ATTRIBUTE_REPARSE_POINT:
            raise RuntimeError('Reparse point rejected')
    return path


def copy_packages():
    source = checked(WORKSPACE / 'bin')
    for directory, folders, files in os.walk(source, followlinks=False):
        checked(directory)
        for name in folders + files:
            checked(Path(directory) / name)
    checked(ROOT).mkdir(parents=True)
    shutil.copytree(source, ROOT / 'bin')


def passed(name):
    RESULTS.append({'name': name, 'passed': True})
    print('PASS', name, flush=True)


class Session:
    def __init__(self):
        env = {'SystemRoot': os.environ.get('SystemRoot', r'C:\Windows'),
               'FLARAGON_DEV_ROOT': str(WORKSPACE),
               'TEMP': str(WORKSPACE / '.cache/tmp'), 'TMP': str(WORKSPACE / '.cache/tmp')}
        self.proc = subprocess.Popen([str(checked(EXE)), str(ROOT)], cwd=ROOT, env=env,
                                     stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                     text=True, encoding='utf-8', creationflags=subprocess.CREATE_NO_WINDOW)
        self.snapshot = None

    def call(self, action, expect=True, **args):
        self.proc.stdin.write(json.dumps(dict(action=action, **args)) + '\n')
        self.proc.stdin.flush()
        response = json.loads(self.proc.stdout.readline())
        self.snapshot = response['snapshot']
        assert response['ok'] == expect, response.get('error', action)
        return response

    def identities(self):
        return {r['id']: r['detail'] for r in self.snapshot['rows'] if r['running']}


def main():
    copy_packages()
    session = Session()
    try:
        session.call('scan')
        config = copy.deepcopy(session.snapshot['config'])
        assert not config['mysql'], 'Discovery must not recreate removed MySQL services'
        packages = session.snapshot['packages']
        assert all(p['error'] is None for p in packages)
        by_version = {p['version']: p for p in packages}
        config['http_port'] = 28080
        config['apache_usage'] = 'common'
        config['default_php'] = by_version['8.3.35']['id']
        for index, php in enumerate(config['php']):
            php.update(port=29100 + index * 8, usage='disabled' if php['package'] == by_version['7.4.33']['id'] else 'normal')
        config['redis'].update(port=26379, usage='common')
        config['mysql'] = [dict(id='5-7-demo', name='demo', series='5.7', package=by_version['5.7.44']['id'], port=23357, usage='common', reuse_data=False),
                           dict(id='8-0-demo', name='demo', series='8.0', package=by_version['8.0.44']['id'], port=23380, usage='normal', reuse_data=False)]
        site = checked(ROOT / 'www/中文 测试/public')
        site.mkdir(parents=True)
        (site / 'index.php').write_text('<?php echo PHP_VERSION;', encoding='utf-8')
        config['projects'] = [dict(id='site', domain='site.localhost', directory='www/中文 测试', document_root='public', php=None, enabled=True)]
        session.call('apply', config=config)
        session.call('start_common')
        common_ids = session.identities()
        assert set(common_ids) == {'mysql:5-7-demo', 'redis', 'apache', 'php:' + by_version['8.3.35']['id']}
        passed('Common starts common services and ordinary PHP dependencies, excludes disabled services')

        for kind in ('apache', 'php', 'mysql', 'redis'):
            candidate = copy.deepcopy(session.snapshot['config'])
            if kind == 'apache':
                candidate['http_port'] += 1
            elif kind == 'php':
                next(p for p in candidate['php'] if p['package'] == by_version['8.3.35']['id'])['workers'] = 3
            elif kind == 'mysql':
                candidate['mysql'] = candidate['mysql'][1:]
            else:
                candidate['redis']['usage'] = 'disabled'
            rejected = session.call('settings', expect=False, config=candidate)
            assert '正在运行' in rejected['error'] or '运行中的网站' in rejected['error']
            assert session.identities() == common_ids
        passed('Live Apache/PHP/MySQL/Redis settings reject editing, removal and disabling')

        candidate = copy.deepcopy(session.snapshot['config'])
        candidate['mysql'][1]['port'] = 23381
        session.call('settings', config=candidate)
        assert session.identities() == common_ids
        passed('Saving a stopped service does not restart other running services')
        session.call('start_all')
        assert len(session.identities()) == 6
        assert 'php:' + by_version['7.4.33']['id'] not in session.identities()
        assert checked(ROOT / 'data/mysql/5.7/demo/mysql').is_dir()
        assert checked(ROOT / 'data/mysql/8.0/demo/mysql').is_dir()
        passed('All starts ordinary services too; same-name 5.7 and 8.0 databases run in separate directories')
        session.call('stop_all')
        assert not session.identities()

        candidate = copy.deepcopy(session.snapshot['config'])
        duplicate = copy.deepcopy(candidate['mysql'][0]); duplicate.update(id='duplicate', name='DEMO', port=23359)
        candidate['mysql'].append(duplicate)
        rejected = session.call('settings', expect=False, config=candidate)
        assert '不能被多个服务使用' in rejected['error']
        passed('Same-series names are rejected case-insensitively')

        original = copy.deepcopy(session.snapshot['config'])
        data = checked(ROOT / 'data/mysql/5.7/demo')
        sentinel = data / 'preserve.txt'
        sentinel.write_text('history survives removal', encoding='utf-8')
        candidate = copy.deepcopy(original); candidate['mysql'] = candidate['mysql'][1:]
        session.call('settings', config=candidate)
        session.call('scan')
        assert len(session.snapshot['config']['mysql']) == 1
        assert sentinel.read_text() == 'history survives removal'
        rejected = session.call('settings', expect=False, config=original)
        assert '确认使用历史数据' in rejected['error']
        original['mysql'][0]['reuse_data'] = True
        session.call('settings', config=original)
        session.call('start', id='mysql:5-7-demo')
        session.call('stop_all')
        assert sentinel.read_text() == 'history survives removal'
        passed('Removal retains data, scan does not recreate service, confirmed matching history restarts successfully')

        candidate = copy.deepcopy(session.snapshot['config'])
        candidate['mysql'][0]['package'] = by_version['8.0.44']['id']
        rejected = session.call('settings', expect=False, config=candidate)
        assert '不能改名或切换' in rejected['error']
        candidate = copy.deepcopy(session.snapshot['config'])
        next(p for p in candidate['php'] if p['package'] == by_version['8.3.35']['id'])['usage'] = 'disabled'
        session.call('settings', config=candidate)
        rejected = session.call('start_common', expect=False)
        assert '所需 PHP 已停用' in rejected['error'] and not session.identities()
        passed('Version switching and disabled PHP dependencies are rejected before any batch service starts')
    finally:
        try:
            session.call('stop_all')
        finally:
            session.proc.stdin.close()
            session.proc.wait(timeout=20)
            evidence = checked(WORKSPACE / 'testwork/evidence/service-settings.json')
            evidence.parent.mkdir(parents=True, exist_ok=True)
            evidence.write_text(json.dumps({'root': str(ROOT), 'components': ['MySQL 5.7.44', 'MySQL 8.0.44', 'PHP 7.4.33/8.3.35/8.5.11', 'Apache 2.4.69', 'Redis 7.4.3'], 'checks': RESULTS}, ensure_ascii=False, indent=2), encoding='utf-8')


if __name__ == '__main__':
    main()
