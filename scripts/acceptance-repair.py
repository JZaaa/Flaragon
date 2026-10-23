"""Validate the repair EXE against fresh original installations inside the workspace."""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import uuid

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location('builder', ROOT / 'scripts/package.py')
builder = importlib.util.module_from_spec(spec)
spec.loader.exec_module(builder)
checked = builder.checked


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--repair', type=Path, required=True)
    args = parser.parse_args()
    assert os.environ.get('FLARAGON_DEV_ROOT', '').lower() == str(ROOT).lower()
    repair = checked(args.repair.absolute())
    update = checked(repair / 'update.exe')
    manifest = json.loads(checked(repair / 'release.json').read_bytes())
    version = manifest['version']
    release = checked(ROOT / 'dist' / version)
    setup = checked(release / f'Flaragon-Setup-{version}-x64.exe')
    original_manifest = json.loads(checked(release / 'release.json').read_bytes())
    assert json.loads(checked(repair / 'payload/repair-base.json').read_bytes()) == original_manifest
    base = checked(ROOT / 'testwork' / ('repair-' + uuid.uuid4().hex[:12]))
    base.mkdir()
    original_update = checked(base / 'ordinary-update.exe')
    original_files = {name: checked(release / 'payload' / name).read_bytes()
                      for name in [*original_manifest['files'], 'release.json']}
    builder.write_package(
        checked(ROOT / 'target/x86_64-pc-windows-msvc/release/flaragon-deploy.exe').read_bytes(),
        builder.archive(original_files), original_update, 1)
    results = []

    def call(exe, action, target, ok=True):
        result = subprocess.run([str(checked(exe)), action, str(checked(target))],
            capture_output=True, encoding='utf-8', timeout=90, creationflags=subprocess.CREATE_NO_WINDOW)
        response = json.loads(result.stdout.strip())
        assert response['ok'] == ok and (result.returncode == 0) == ok, response
        return response

    def program(target):
        return {name: checked(target / name).read_bytes()
                for name in [*manifest['files'], 'release.json', '.flaragon-install.json']}

    first, second, gui = [checked(base / name) for name in ['修复 中文 空格', 'independent', 'gui-target']]
    for target in [first, second, gui]:
        call(setup, '--apply', target)
    second_before = program(second)
    call(original_update, '--inspect', first, False)
    call(update, '--inspect', base / 'not-installed', False)
    call(update, '--inspect', ROOT, False)
    results.append('Repair requires an installed target; ordinary same-version update remains rejected')
    cli = checked(ROOT / 'target/x86_64-pc-windows-msvc/release/flaragon-cli.exe')
    process = subprocess.Popen([str(cli), str(first)], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
        stderr=subprocess.PIPE, encoding='utf-8', creationflags=subprocess.CREATE_NO_WINDOW)
    try:
        process.stdin.write('{"action":"snapshot"}\n'); process.stdin.flush()
        snapshot = json.loads(process.stdout.readline())
        assert snapshot['ok']
        before = program(first)
        call(update, '--apply', first, False)
        assert program(first) == before
    finally:
        process.stdin.close()
        process.wait(timeout=20)
    results.append('Owned manager lock rejects repair without changing program files')
    original_config = json.dumps(snapshot['snapshot']['config'], ensure_ascii=False).encode('utf-8')
    sentinels = {'config/app.json': original_config, 'data/mysql/keep.bin': b'database fixture',
        'www/keep.php': b'<?php echo 1;', 'bin/php/keep.txt': b'component fixture',
        'runtime/temp/keep.session': b'session fixture',
        'config/startup.json': b'{"launch_at_login":false,"start_common_services":true}'}
    for name, data in sentinels.items():
        path = checked(first / name); path.parent.mkdir(parents=True, exist_ok=True); path.write_bytes(data)
    preserved = {name: (checked(first / name).read_bytes(), checked(first / name).stat().st_mtime_ns)
                 for name in [*sentinels, '.flaragon-install.json']}
    call(update, '--inspect', first)
    call(update, '--apply', first)
    assert json.loads(checked(first / 'release.json').read_bytes()) == manifest
    for name in manifest['files']:
        assert checked(first / name).read_bytes() == checked(repair / 'payload' / name).read_bytes()
    assert all((checked(first / name).read_bytes(), checked(first / name).stat().st_mtime_ns) == value
               for name, value in preserved.items())
    assert program(second) == second_before
    results.append('Same-version real EXE repair preserves user bytes, mtimes, identity and independent installation')
    repaired = program(first)
    call(update, '--apply', first, False)
    call(original_update, '--apply', first, False)
    assert program(first) == repaired
    wrong = dict(original_manifest, release_id='a' * 32)
    checked(second / 'release.json').write_text(json.dumps(wrong), encoding='utf-8')
    wrong_before = program(second)
    call(update, '--apply', second, False)
    assert program(second) == wrong_before
    checked(second / 'release.json').write_bytes(second_before['release.json'])
    results.append('Repeated repair, ordinary rollback and another build identity are rejected')
    token = uuid.uuid4().hex
    stage = checked(second / '.maintenance' / token / 'new')
    for name in [*manifest['files'], 'release.json']:
        path = checked(stage / name); path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(checked(repair / 'payload' / name).read_bytes())
    journal = dict(format=1, token=token, mode='Repair',
        installation=json.loads(checked(second / '.flaragon-install.json').read_bytes()),
        previous=original_manifest, next=manifest)
    checked(second / '.maintenance/active.json').write_text(json.dumps(journal), encoding='utf-8')
    checked(second / 'Flaragon.exe').write_bytes(checked(repair / 'payload/Flaragon.exe').read_bytes())
    call(update, '--apply', second)
    assert json.loads(checked(second / 'release.json').read_bytes()) == manifest
    assert not checked(second / '.maintenance/active.json').exists()
    results.append('Real repair EXE completes an interrupted transaction with a replaced main executable')
    evidence = dict(product_version=manifest['version'], official_release=False, repair=str(update),
        gui_target=str(gui), fixture_root=str(base), results=results, stopped=True)
    path = checked(ROOT / 'testwork/evidence' / ('repair-' + repair.name + '.json'))
    path.write_text(json.dumps(evidence, ensure_ascii=False, indent=2), encoding='utf-8')
    print(json.dumps(evidence, ensure_ascii=False, indent=2))


if __name__ == '__main__':
    main()
