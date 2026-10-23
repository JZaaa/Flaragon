"""Check complete PHP runtime versions with owned Apache/FastCGI instances."""
import copy
import importlib.util
import json
import os
from pathlib import Path
import shutil
import socket

spec = importlib.util.spec_from_file_location(
    'web_entries', Path(__file__).with_name('acceptance-web-entries.py'))
web = importlib.util.module_from_spec(spec)
spec.loader.exec_module(web)
CUSTOM = 'php-7.9.99-p261023-Win32-vc15-x64-TS'
VERSIONS = {
    CUSTOM: '7.9.99-p261023',
    'php-7.4.33': '7.4.33',
    'php-8.3.35': '8.3.35',
    'php-8.5.11': '8.5.11',
}


def copy_package(component, name):
    source = web.checked(web.WORKSPACE / 'bin' / component / name)
    for directory, folders, files in os.walk(source, followlinks=False):
        web.checked(directory)
        for name in folders + files:
            web.checked(Path(directory) / name)
    target = web.checked(web.ROOT / 'bin' / component / source.name)
    target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copytree(source, target)


def free_ports(count):
    held = []
    try:
        for _ in range(count):
            sock = socket.socket()
            held.append(sock)
            sock.bind(('127.0.0.1', 0))
        return [sock.getsockname()[1] for sock in held]
    finally:
        for sock in held:
            sock.close()


def main():
    web.checked(web.ROOT).mkdir(parents=True)
    copy_package('apache', 'apache-2.4.69')
    for name in VERSIONS:
        copy_package('php', name)
    session = web.Session()
    results = []
    try:
        session.call('scan')
        packages = {p['id']: p for p in session.snapshot['packages']}
        assert all(p['error'] is None for p in packages.values()), packages
        php_packages = {p['id'] for p in packages.values() if p['kind'] == 'php'}
        assert len(php_packages) == len(VERSIONS), packages
        settings = session.snapshot['config']['php']
        assert {p['package'] for p in settings} == php_packages, settings
        assert len(settings) == len(php_packages), settings
        for package in packages.values():
            if package['kind'] == 'php':
                assert package['version'] == VERSIONS[package['name']], package
                assert package['php_thread_safe'] == (package['name'] == CUSTOM), package
        session.call('scan')
        assert session.snapshot['config']['php'] == settings
        results.append('Custom TS and PHP 7.4/8.3/8.5 NTS packages are discovered and appear once in settings after repeated scans')
        config = copy.deepcopy(session.snapshot['config'])
        ports = iter(free_ports(1 + 2 * len(config['php'])))
        config.update(http_port=next(ports), http_address='127.0.0.1')
        config['projects'] = []
        for index, php in enumerate(config['php']):
            directory = f'www/php-{index}'
            php.update(port=next(ports), workers=1, usage='normal', directory_entry=dict(
                enabled=True, port=next(ports), directory=directory, address='127.0.0.1'))
            web.write(web.ROOT / directory / 'index.php',
                      "<?php header('Content-Type: application/json'); echo json_encode("
                      "['version'=>PHP_VERSION,'ini'=>php_ini_loaded_file(),'zts'=>PHP_ZTS]);")
            config['projects'].append(dict(id=f'site-{index}', name=f'PHP {index}',
                directory=directory, document_root='', php=php['package'],
                enabled=True, listed=True, lan_access=False, domain=f'site-{index}.localhost'))
        session.call('apply', config=config)
        session.call('start', id='apache')
        session.call('verify')
        responses = []
        for index, php in enumerate(config['php']):
            for port, route in [(php['directory_entry']['port'], '/'),
                                (config['http_port'], f'/site-{index}/')]:
                status, _, body = session.get(port, route)
                data = json.loads(body)
                assert status == 200 and data['version'] == packages[php['package']]['version'], body
                assert web.checked(data['ini']).is_relative_to(web.ROOT)
                responses.append(dict(port=port, route=route, **data))
        results.append('Directory and managed project entries return complete runtime versions and owned ini paths')
        health_files = [web.checked(p) for p in web.checked(web.ROOT / 'runtime').rglob('health.php')]
        assert len(health_files) == 1, health_files
        health = health_files[0]
        original = health.read_bytes()
        try:
            health.write_bytes(original.replace(
                b"'version'=>PHP_VERSION",
                b"'version'=>(PHP_VERSION==='7.9.99-p261023'?'7.9.99-other':PHP_VERSION)"))
            mismatch = session.call('verify', expect=False)['error']
            assert '预期 7.9.99-p261023' in mismatch and '实际 7.9.99-other' in mismatch, mismatch
            assert '入口 PHP 版本不符' in mismatch, mismatch
        finally:
            health.write_bytes(original)
        session.call('verify')
        results.append('A different version suffix is rejected with expected and actual values; restored response passes')
        session.call('reload', id='apache')
        session.call('stop_all')
        session.call('start', id='apache')
        session.call('verify')
        results.append('Apache reload and full service restart pass runtime verification')
    finally:
        try:
            session.call('stop_all')
            assert not any(r['running'] for r in session.snapshot['rows'])
        finally:
            session.process.stdin.close()
            session.process.wait(timeout=15)
    evidence = dict(environment=str(web.ROOT), executable=str(web.EXE),
                    packages=list(packages.values()), responses=responses,
                    mismatch=mismatch, results=results, stopped=True)
    web.write(web.WORKSPACE / 'testwork/evidence/php-version.json',
              json.dumps(evidence, ensure_ascii=False, indent=2))
    print(json.dumps(dict(results=results, responses=responses, mismatch=mismatch,
                         stopped=True), ensure_ascii=False, indent=2))


if __name__ == '__main__':
    main()
