"""Validate release installation and upgrades in new workspace-only fixtures."""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tomllib
import uuid

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location('builder', ROOT / 'scripts/package.py')
builder = importlib.util.module_from_spec(spec)
spec.loader.exec_module(builder)
checked = builder.checked


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--baseline', required=True)
    parser.add_argument('--rejected-baseline', action='append', default=[], help='Older release that this updater must reject')
    parser.add_argument('--repair', type=Path, action='append', help='Apply baseline repairs in the supplied order')
    parser.add_argument('--baseline-release', type=Path, help='Archived same-version rebuild used as the upgrade baseline')
    parser.add_argument('--release', type=Path, help='Separate rebuild archive to validate')
    args = parser.parse_args()
    assert os.environ.get('FLARAGON_DEV_ROOT', '').lower() == str(ROOT).lower()
    current = tomllib.loads(checked(ROOT / 'Cargo.toml').read_text(encoding='utf-8'))['package']['version']
    assert tuple(map(int, args.baseline.split('.'))) < tuple(map(int, current.split('.')))
    release = checked(args.release.absolute()) if args.release else checked(ROOT / 'dist' / current)
    manifest = json.loads(checked(release / 'release.json').read_bytes())
    assert manifest['version'] == current
    assert manifest['architecture'] == 'x86_64-pc-windows-msvc'
    expected = {'Flaragon.exe', 'lib/Scintilla.dll', 'lib/Lexilla.dll'}
    assert expected.issubset(manifest['files'])
    assert all(name in expected or name.startswith('licenses/') for name in manifest['files'])
    setup = checked(release / f'Flaragon-Setup-{current}-x64.exe')
    update = checked(release / f'Flaragon-Update-{current}-x64.exe')
    old_release = checked(args.baseline_release.absolute()) if args.baseline_release else checked(ROOT / 'dist' / args.baseline)
    old_setup = checked(old_release / f'Flaragon-Setup-{args.baseline}-x64.exe')
    old_update = checked(old_release / f'Flaragon-Update-{args.baseline}-x64.exe')
    cli = checked(ROOT / 'target/x86_64-pc-windows-msvc/release/flaragon-cli.exe')
    base = checked(ROOT / 'testwork' / ('release-' + current + '-' + uuid.uuid4().hex[:12]))
    base.mkdir(parents=True)
    results = []

    def call(exe, action, target, ok=True):
        result = subprocess.run([str(checked(exe)), action, str(checked(target))],
            capture_output=True, encoding='utf-8', timeout=90, creationflags=subprocess.CREATE_NO_WINDOW)
        response = json.loads(result.stdout.strip())
        assert response['ok'] == ok and (result.returncode == 0) == ok, response
        return response

    def record(name):
        results.append(name)
        print('PASS', name, flush=True)

    def write(target, name, data):
        path = checked(target / name)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)

    def program(target):
        installed = json.loads(checked(target / 'release.json').read_bytes())
        return {name: checked(target / name).read_bytes()
                for name in [*installed['files'], 'release.json', '.flaragon-install.json']}

    def verify(target):
        assert json.loads(checked(target / 'release.json').read_bytes()) == manifest
        for name in manifest['files']:
            assert checked(target / name).read_bytes() == checked(release / 'payload' / name).read_bytes()
        subprocess.run([str(checked(target / 'Flaragon.exe')), '--deployment-check', current, str(target)],
            check=True, timeout=25, creationflags=subprocess.CREATE_NO_WINDOW)

    fresh, old, independent, resume = [base / name for name in ['新安装 中文 空格', 'original', 'independent', 'resume']]
    call(setup, '--inspect', fresh)
    assert not fresh.exists()
    call(setup, '--apply', fresh)
    verify(fresh)
    assert not checked(fresh / 'data').exists() and not checked(fresh / 'config').exists()
    assert all(not any(checked(fresh / 'bin' / kind).iterdir()) for kind in ['apache', 'php', 'mysql', 'redis'])
    record('software-only fresh installation, exact payload and executable self-check')
    for target in [old, independent, resume]:
        call(old_setup, '--apply', target)
    independent_before = program(independent)
    assert checked(old / '.flaragon-install.json').read_bytes() != checked(independent / '.flaragon-install.json').read_bytes()
    process = subprocess.Popen([str(cli), str(old)], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
        stderr=subprocess.PIPE, encoding='utf-8', creationflags=subprocess.CREATE_NO_WINDOW)
    try:
        process.stdin.write('{"action":"snapshot"}\n'); process.stdin.flush()
        response = json.loads(process.stdout.readline())
        assert response['ok']
        before = program(old)
        call(update, '--apply', old, False)
        assert before == program(old) and process.poll() is None
    finally:
        process.stdin.close(); process.wait(timeout=20)
    record('running manager lock refuses upgrade without replacement or stopping process')
    targets = [old]
    if args.repair:
        repaired = base / 'repaired'
        call(old_setup, '--apply', repaired)
        for repair in args.repair:
            call(checked(repair.absolute()), '--apply', repaired)
        targets.append(repaired)
    for target in targets:
        sentinels = {'config/app.json': json.dumps(response['snapshot']['config']).encode(),
            'config/startup.json': b'{"launch_at_login":false,"start_common_services":true}',
            'data/mysql/keep.bin': b'isolated database fixture', 'www/keep.php': b'<?php echo 1;',
            'bin/php/keep.txt': b'component fixture', 'runtime/temp/keep.session': b'session fixture'}
        for name, data in sentinels.items():
            write(target, name, data)
        preserved = {name: (checked(target / name).read_bytes(), checked(target / name).stat().st_mtime_ns)
                     for name in [*sentinels, '.flaragon-install.json']}
        call(update, '--inspect', target)
        call(update, '--apply', target)
        verify(target)
        assert all((checked(target / name).read_bytes(), checked(target / name).stat().st_mtime_ns) == value
                   for name, value in preserved.items())
        assert program(independent) == independent_before
        record(f'{target.name} baseline upgrade preserves bytes, mtimes, identity and other installation')
    before = program(old)
    call(update, '--apply', old, False)
    if old_update.exists():
        call(old_update, '--apply', old, False)
        record('archived older updater refuses downgrade')
    call(setup, '--apply', old, False)
    assert before == program(old)
    call(update, '--inspect', ROOT, False)
    unknown = base / 'unknown'
    write(unknown, 'keep.txt', b'unknown target fixture')
    call(setup, '--apply', unknown, False)
    call(update, '--inspect', unknown, False)
    assert checked(unknown / 'keep.txt').read_bytes() == b'unknown target fixture'
    record('same-version upgrade, reinstall, unknown target and source directory rejected')
    token = uuid.uuid4().hex
    journal = dict(format=1, token=token, mode='Update',
        installation=json.loads(checked(resume / '.flaragon-install.json').read_bytes()),
        previous=json.loads(checked(resume / 'release.json').read_bytes()), next=manifest)
    for name in [*manifest['files'], 'release.json']:
        write(resume, f'.maintenance/{token}/new/{name}', checked(release / 'payload' / name).read_bytes())
    write(resume, '.maintenance/active.json', json.dumps(journal).encode())
    write(resume, 'Flaragon.exe', checked(release / 'payload/Flaragon.exe').read_bytes())
    call(update, '--apply', resume)
    verify(resume)
    assert not checked(resume / '.maintenance/active.json').exists()
    record('same release completes interrupted upgrade with main executable already replaced')
    for rejected in args.rejected_baseline:
        assert len(rejected.split('.')) == 3 and all(p.isascii() and p.isdigit() for p in rejected.split('.'))
        rejected_setup = checked(ROOT / 'dist' / rejected / f'Flaragon-Setup-{rejected}-x64.exe')
        target = base / ('rejected-' + rejected)
        call(rejected_setup, '--apply', target)
        before = program(target)
        for action in ['--inspect', '--apply']:
            result = call(update, action, target, False)
            assert '上一正式版本' in result['error'], result
        assert before == program(target) and not checked(target / '.maintenance/active.json').exists()
        record(f'non-predecessor {rejected} rejected before replacement')
    evidence = dict(version=current, release=str(release), release_id=manifest['release_id'],
        baseline=args.baseline, baseline_release=str(old_release),
        repairs=[str(checked(p.absolute())) for p in (args.repair or [])], fixture_root=str(base), checks=results,
        rejected_baselines=args.rejected_baseline,
        stopped=True, limits=['User files are independent sentinels; no database compatibility claim',
                              'Cross-machine, DPI and actual power-loss faults not tested'])
    evidence_name = (release.name if args.release else current) + '-from-' + old_release.name
    if args.repair:
        evidence_name += '-' + args.repair[-1].parent.name
    write(ROOT, f'testwork/evidence/release-{evidence_name}.json', json.dumps(evidence, ensure_ascii=False, indent=2).encode())
    print(json.dumps(evidence, ensure_ascii=False, indent=2))


if __name__ == '__main__':
    main()
