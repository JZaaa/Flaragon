"""Build an explicitly requested same-version repair without changing release archives."""
import argparse
import importlib.util
import json
import os
import re
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
    parser.add_argument('--repair-id', required=True)
    parser.add_argument('--description', required=True)
    parser.add_argument('--base-repair-id', help='Bind to the resulting build of an archived repair')
    args = parser.parse_args()
    assert os.environ.get('FLARAGON_DEV_ROOT', '').lower() == str(ROOT).lower()
    version = tomllib.loads(checked(ROOT / 'Cargo.toml').read_text(encoding='utf-8'))['package']['version']
    assert re.fullmatch(r'[a-z][a-z0-9.-]{0,79}', args.repair_id), 'Invalid repair identifier'
    assert args.repair_id.endswith('-' + version), 'Repair identifier must end with the product version'
    assert args.description.strip(), 'A repair description is required'
    release = checked(ROOT / 'dist' / version)
    if args.base_repair_id:
        assert re.fullmatch(r'[a-z][a-z0-9.-]{0,79}', args.base_repair_id), 'Invalid baseline repair identifier'
        assert args.base_repair_id.endswith('-' + version), 'Baseline repair version mismatch'
        assert args.base_repair_id != args.repair_id, 'Repair cannot replace its own baseline archive'
        release = checked(ROOT / 'dist/repairs' / args.base_repair_id)
        info = json.loads(checked(release / 'build-info.json').read_bytes())
        assert info['repair_id'] == args.base_repair_id and info['product_version'] == version
    base_bytes = checked(release / 'release.json').read_bytes()
    base = json.loads(base_bytes)
    assert base['version'] == version and base['product'] == 'Flaragon', 'Baseline product/version mismatch'
    if args.base_repair_id:
        assert info['repair_build_id'] == base['release_id'], 'Baseline repair identity mismatch'
    files = {name: checked(release / 'payload' / name).read_bytes() for name in base['files']}
    assert all(len(files[name]) == size for name, size in base['files'].items())
    main_exe = checked(ROOT / 'target/x86_64-pc-windows-msvc/release/Flaragon.exe').read_bytes()
    assert main_exe != files['Flaragon.exe'], 'Repair must contain a changed program'
    files['Flaragon.exe'] = main_exe
    output = checked(ROOT / 'dist/repairs' / args.repair_id)
    # An existing repair is immutable; reruns may only reproduce its exact payload.
    previous = json.loads(checked(output / 'release.json').read_text(encoding='utf-8')) if output.exists() else None
    manifest = dict(base, release_id=previous['release_id'] if previous else uuid.uuid4().hex,
                    files={name: len(data) for name, data in sorted(files.items())})
    files['release.json'] = json.dumps(manifest, ensure_ascii=False, indent=2).encode('utf-8')
    files['repair-base.json'] = base_bytes
    stub = checked(ROOT / 'target/x86_64-pc-windows-msvc/release/flaragon-deploy.exe').read_bytes()
    payload = builder.archive(files)
    if previous:
        import struct
        expected = stub + payload + struct.pack('<QB', len(payload), 2) + builder.MAGIC
        assert checked(output / 'update.exe').read_bytes() == expected, 'Existing repair contents differ'
        print(output / 'update.exe')
        return
    output.mkdir(parents=True)
    stage = checked(output / 'payload')
    for name, data in files.items():
        path = checked(stage / name)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
    subprocess.run([str(stage / 'Flaragon.exe'), '--deployment-check', version, str(stage)],
                   check=True, timeout=25, creationflags=subprocess.CREATE_NO_WINDOW)
    builder.write_package(stub, payload, output / 'update.exe', 2)
    checked(output / 'release.json').write_bytes(files['release.json'])
    checked(output / 'build-info.json').write_text(json.dumps(dict(
        purpose=args.description, product_version=version, repair_id=args.repair_id,
        base_release_id=base['release_id'], repair_build_id=manifest['release_id'],
        base_repair_id=args.base_repair_id,
        source=f'Cargo.lock and project release build; editor DLLs and licenses from {release.relative_to(ROOT).as_posix()}',
        official_release=False, signed=False), indent=2), encoding='utf-8')
    print(output / 'update.exe')


if __name__ == '__main__':
    main()
