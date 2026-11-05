"""Check older installed test fixtures against the current release updater."""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tomllib
import uuid

spec = importlib.util.spec_from_file_location('builder', Path(__file__).with_name('package.py'))
builder = importlib.util.module_from_spec(spec)
spec.loader.exec_module(builder)
checked = builder.checked
ROOT = builder.ROOT


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--source', type=Path, action='append', required=True,
                        help='Existing project-local installed test fixture; only software files are copied')
    args = parser.parse_args()
    assert os.environ.get('FLARAGON_DEV_ROOT', '').lower() == str(ROOT).lower()
    version = tomllib.loads(checked(ROOT / 'Cargo.toml').read_text(encoding='utf-8'))['package']['version']
    release = checked(ROOT / 'dist' / version)
    manifest = json.loads(checked(release / 'release.json').read_bytes())
    update = checked(release / f'Flaragon-Update-{version}-x64.exe')
    base = checked(ROOT / 'testwork' / ('upgrade-rejection-' + uuid.uuid4().hex[:12]))
    base.mkdir(parents=True)
    results = []
    for index, source in enumerate(args.source):
        source = checked(source.absolute())
        assert source.is_relative_to(ROOT / 'testwork'), 'Only isolated test fixtures are accepted'
        old = json.loads(checked(source / 'release.json').read_bytes())
        assert tuple(map(int, old['version'].split('.'))) < tuple(map(int, manifest['minimum_version'].split('.')))
        target = checked(base / str(index))
        names = [*old['files'], 'release.json', '.flaragon-install.json']
        for name in names:
            destination = checked(target / name)
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes(checked(source / name).read_bytes())
        before = {name: (checked(target / name).read_bytes(), checked(target / name).stat().st_mtime_ns)
                  for name in names}
        responses = []
        for action in ['--inspect', '--apply']:
            run = subprocess.run([str(update), action, str(target)], cwd=ROOT,
                                 capture_output=True, encoding='utf-8', timeout=90,
                                 creationflags=subprocess.CREATE_NO_WINDOW)
            response = json.loads(run.stdout)
            assert run.returncode != 0 and not response['ok'], response
            assert '上一正式版本 ' + manifest['minimum_version'] in response['error'], response
            responses.append(response)
        assert all((checked(target / name).read_bytes(), checked(target / name).stat().st_mtime_ns) == value
                   for name, value in before.items())
        assert not checked(target / '.maintenance/active.json').exists()
        results.append(dict(version=old['version'], source=str(source), target=str(target),
                            responses=responses, program_unchanged=True))
        print('PASS rejected before writes:', old['version'], flush=True)
    evidence = dict(version=version, release_id=manifest['release_id'], results=results)
    output = checked(ROOT / 'testwork/evidence' / f'release-{version}-older-rejection.json')
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(evidence, ensure_ascii=False, indent=2), encoding='utf-8')


if __name__ == '__main__':
    main()
