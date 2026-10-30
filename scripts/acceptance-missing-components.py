"""Validate manual removal and project rebinding with workspace-owned PHP/Apache.

Run with scripts/env.ps1. The default run prepares and stops a fixture. After
moving its PHP 7.4 package outside bin within the fixture, use --verify <root>.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import time
import tomllib

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location('fixture', ROOT / 'scripts/acceptance-package-config.py')
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)


def exercise(root, verify):
    session = fixture.Session(root)
    checks = []
    def record(text):
        checks.append(text)
        print('PASS', text, flush=True)
    try:
        session.call('scan')
        config = session.snapshot['config']
        old = next(s['package'] for s in config['php'] if '7-4-33' in s['package'])
        new = next(s['package'] for s in config['php'] if '8-3-35' in s['package'])
        if not verify:
            config['http_port'] = 28490
            config['default_php'] = old
            for index, php in enumerate(config['php']):
                php.update(port=29500 + 8 * index, workers=1)
            config['projects'] = [dict(id='explicit', name='显式版本', directory='www/explicit', document_root='', php=old, enabled=True),
                                  dict(id='inherited', name='默认版本', directory='www/inherited', document_root='', php=None, enabled=True)]
            for project in config['projects']:
                fixture.write(root / project['directory'] / 'index.php', "<?php header('Content-Type: application/json'); echo json_encode(['version'=>PHP_VERSION]);")
            session.call('apply', config=config)
            session.call('start_all')
            session.call('verify')
            record('Original PHP and Apache configuration starts and verifies')
        else:
            assert next(r for r in session.snapshot['rows'] if r['id'] == 'php:' + old)['missing']
            before = (root / 'config/app.json').read_bytes()
            result = session.call('remove_missing', expect=False, id='php:' + old)
            assert 'explicit' in result['error'] and 'inherited' in result['error']
            assert (root / 'config/app.json').read_bytes() == before
            record('Referenced missing PHP cannot be removed and saved configuration is unchanged')
            config['projects'][0]['php'] = new
            session.call('apply', config=config)
            assert '尚未校验' in session.snapshot['message']
            record('One project can switch PHP while another still references the missing package')
            config = session.snapshot['config']
            config['default_php'] = new
            session.call('settings', config=config)
            session.call('remove_missing', id='php:' + old)
            assert all(p['package'] != old for p in session.snapshot['config']['php'])
            session.call('remove_missing', expect=False, id='php:' + new)
            session.call('scan')
            assert all(r['id'] != 'php:' + old for r in session.snapshot['rows'])
            record('Manual removal persists across refresh and refuses an existing PHP package')
            session.call('start_all')
            session.call('verify')
            for project in session.snapshot['config']['projects']:
                assert fixture.http(session, project)['version'] == '8.3.35'
            record('Start all and both websites succeed with the remaining PHP 8.3 package')
            assert not (root / 'bin/php/php-7.4.33').exists()
            assert (root / 'retired-php/php.ini').exists()
            record('Removed package is not recreated and retained configuration remains untouched')
    finally:
        session.close()
    evidence = dict(version=tomllib.loads((ROOT / 'Cargo.toml').read_text(encoding='utf-8'))['package']['version'],
                    executable=str(fixture.EXE), build_modified=fixture.EXE.stat().st_mtime_ns,
                    root=str(root), checks=checks, stopped=True)
    fixture.write(root / ('acceptance-removal.json' if verify else 'acceptance-prepare.json'), json.dumps(evidence, ensure_ascii=False, indent=2))


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--verify', type=Path)
    args = parser.parse_args()
    assert Path(os.environ['FLARAGON_DEV_ROOT']).resolve() == ROOT
    if args.verify:
        root = fixture.checked(args.verify)
    else:
        root = fixture.checked(ROOT / 'testwork' / ('missing-components-' + str(time.time_ns())))
        root.mkdir()
        for kind, names in {'php': ['php-7.4.33', 'php-8.3.35'], 'apache': ['apache-2.4.69']}.items():
            for name in names:
                fixture.copy_package(ROOT / 'bin' / kind / name, root / 'bin' / kind / name)
        (root / 'www').mkdir()
        (root / 'runtime/temp').mkdir(parents=True)
        fixture.write(ROOT / 'testwork/missing-components-latest.txt', str(root))
    exercise(root, bool(args.verify))
