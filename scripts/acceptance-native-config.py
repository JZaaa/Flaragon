"""Verify native file editing against project-owned components and fresh fixture content."""
import json
from pathlib import Path
from http.client import HTTPConnection
import sys
sys.path.insert(0, str(Path(__file__).resolve().parent))
from acceptance import Session, ROOT, EVIDENCE, mysql, redis, http, user_config

session = Session()
results = []
originals = {}
package_files = {}


def passed(name):
    results.append(dict(name=name, passed=True))
    print('PASS', name, flush=True)


def edit(service_id, addition):
    native = session.call('read_config', id=service_id)
    originals.setdefault(service_id, user_config(native['text']))
    text = native['text']
    marker = '# BEGIN FLARAGON PROJECTS'
    text = text.replace(marker, addition + '\n' + marker) if marker in text else text + '\n' + addition + '\n'
    session.call('save_config', id=service_id, text=text)
    return Path(native['path'])


def assert_native_process_paths():
    state = json.loads((ROOT / 'runtime/state/processes.json').read_text(encoding='utf-8'))
    for record in state['records']:
        native = session.call('read_config', id=record['id'])
        assert ROOT / record['config'] == Path(native['path'])
        assert Path(native['path']).resolve().is_relative_to(ROOT / ('config/services/apache' if record['id'] == 'apache' else 'bin'))


try:
    session.call('scan')
    session.call('stop_all')
    config = session.snapshot['config']
    packages = {p['id']: p for p in session.snapshot['packages']}
    for package in packages.values():
        directory = Path(package['directory']).resolve()
        assert directory.is_relative_to(ROOT)
        for relative in ['php.ini-development', 'php.ini-production', 'my.ini', 'conf/httpd.conf']:
            file = directory / relative
            assert file.resolve().is_relative_to(ROOT)
            if file.is_file():
                package_files[file] = file.read_bytes()
    session.call('apply', config=config)
    template_counts = {}
    for service_id, source, minimum in [
        *[('php:' + p['package'], Path(packages[p['package']]['directory']) / 'php.ini-development', 1500) for p in config['php']],
        ('apache', Path(packages[config['apache']]['directory']) / 'conf/httpd.conf', 500),
        ('redis', Path(packages[config['redis']['package']]['directory']) / 'redis.conf', 2000),
    ]:
        native = session.call('read_config', id=service_id)
        full = native['text']
        assert len(full.splitlines()) >= minimum, service_id
        assert '模板来源：' in full
        comment = ';' if service_id.startswith('php:') else '#'
        for line in source.read_text(encoding='utf-8-sig').splitlines():
            if line.strip().startswith(comment):
                assert line in full, (service_id, line)
        assert session.call('read_config', id=service_id)['text'] == full
        template_counts[service_id] = len(full.splitlines())
    for instance in config['mysql']:
        assert '未附带配置模板' in session.call('read_config', id='mysql:' + instance['id'])['text']
    passed('Package templates retain their complete comments; repeated reads are stable; MySQL missing templates are explicit')
    (EVIDENCE / 'native-template-lines.json').write_text(json.dumps(template_counts, indent=2), encoding='utf-8')
    session.call('start_all')
    assert_native_process_paths()
    passed('All running services load the same persistent native file exposed by the editor')

    for php in config['php']:
        path = edit('php:' + php['package'], '; 保留中文注释\nprecision=12')
        project = next(p for p in config['projects'] if p['php'] == php['package'])
        document = (ROOT / project['directory'] / project['document_root']).resolve()
        assert document.is_relative_to(ROOT)
        (document / 'native-config.php').write_text("<?php header('Content-Type: application/json'); echo json_encode(['ini'=>php_ini_loaded_file(),'precision'=>ini_get('precision')]);", encoding='utf-8')
        result = json.loads(http(project['domain'], '/native-config.php'))
        assert Path(result['ini']) == path and result['precision'] == '12'
    passed('PHP 7.4/8.3/8.5 expose the editor file via php_ini_loaded_file and accept precision outside the old option list')

    sql = config['mysql'][0]
    peer = next(m for m in config['mysql'] if m['id'] != sql['id'] and m['package'] == sql['package'])
    before = mysql(packages[peer['package']], peer['port'], 'SELECT @@connect_timeout;')
    edit('mysql:' + sql['id'], '# 独立实例\nconnect_timeout=17\nskip-show-database')
    assert mysql(packages[sql['package']], sql['port'], 'SELECT @@connect_timeout;') == '17'
    assert mysql(packages[peer['package']], peer['port'], 'SELECT @@connect_timeout;') == before
    passed('Native MySQL boolean and scalar options apply without modifying another instance using the same package')

    edit('redis', '# 原生 Redis 参数\nhz 13')
    assert '13' in redis('CONFIG', 'GET', 'hz')
    edit('apache', '# 原生 Apache 参数\nServerTokens Prod')
    connection = HTTPConnection('127.0.0.1', config['http_port'], timeout=10)
    connection.request('GET', '/', headers={'Host': config['projects'][0]['domain']})
    response = connection.getresponse()
    assert response.getheader('Server') == 'Apache'
    response.read()
    connection.close()
    passed('Redis hz and Apache ServerTokens are accepted and verified on the owned running services')

    for service_id, bad in [('apache', 'DefinitelyInvalidDirective On'), ('mysql:' + sql['id'], 'definitely_invalid_option=1'), ('redis', 'definitely_invalid_option yes')]:
        native = session.call('read_config', id=service_id)
        before = native['text']
        text = before.replace('# BEGIN FLARAGON PROJECTS', bad + '\n# BEGIN FLARAGON PROJECTS') if service_id == 'apache' else before + '\n' + bad + '\n'
        session.call('save_config', id=service_id, text=text, expect=False)
        after = session.call('read_config', id=service_id)['text']
        assert after == before
        session.call('verify')
        assert all(r['state'] == '运行中' for r in session.snapshot['rows'])
    passed('Apache/MySQL syntax errors and Redis startup failure restore the native file and working services')

    native = session.call('read_config', id='mysql:' + sql['id'])
    session.call('save_config', id='mysql:' + sql['id'], text=native['text'].replace('port=' + str(sql['port']), 'port=3306'), expect=False)
    assert session.call('read_config', id='mysql:' + sql['id'])['text'] == native['text']
    passed('Editing a managed instance field fails before changing the file or database')

    session.call('stop_all')
    session.close()
    session = Session()
    session.call('scan')
    session.call('start_all')
    assert_native_process_paths()
    assert '13' in redis('CONFIG', 'GET', 'hz')
    assert mysql(packages[sql['package']], sql['port'], 'SELECT @@connect_timeout;') == '17'
    for php in config['php']:
        native = session.call('read_config', id='php:' + php['package'])['text']
        assert '; 保留中文注释' in native and 'precision=12' in native
    assert all(file.read_bytes() == original for file, original in package_files.items())
    passed('Native comments and settings survive manager restart; source templates remain unchanged')
finally:
    try:
        session.call('stop_all')
        for service_id, text in originals.items():
            session.call('override', id=service_id, text=text)
    finally:
        session.close()
        (EVIDENCE / 'acceptance-native-config.json').write_text(json.dumps(results, ensure_ascii=False, indent=2), encoding='utf-8')
