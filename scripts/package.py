"""Build the two offline deployment EXEs from a strict, project-local payload allowlist."""
from pathlib import Path
import argparse
import gzip
import io
import json
import os
import re
import struct
import subprocess
import tarfile
import tomllib
import uuid

ROOT = Path(__file__).resolve().parent.parent
MAGIC = b'FLARAGONPACK0001'


def checked(path):
    path = Path(path)
    if not path.is_absolute() or '..' in path.parts or not path.is_relative_to(ROOT):
        raise ValueError(f'Path outside workspace: {path}')
    for parent in reversed((path, *path.parents)):
        if parent.is_symlink() or parent.is_junction():
            raise ValueError(f'Redirected path: {parent}')
    return path


def build_payload(version, release_id):
    build = checked(ROOT / 'target/x86_64-pc-windows-msvc/release')
    files = {'Flaragon.exe': checked(build / 'Flaragon.exe').read_bytes()}
    for name in ('Scintilla.dll', 'Lexilla.dll'):
        files[f'lib/{name}'] = checked(ROOT / 'lib' / name).read_bytes()
    for folder, dirs, names in os.walk(checked(ROOT / 'licenses'), followlinks=False):
        for name in dirs:
            checked(Path(folder) / name)
        for name in names:
            path = checked(Path(folder) / name)
            files[path.relative_to(ROOT).as_posix()] = path.read_bytes()
    policy = json.loads(checked(ROOT / 'release-policy.json').read_text(encoding='utf-8'))
    manifest = {'product': 'Flaragon', 'version': version, 'release_id': release_id,
                'architecture': 'x86_64-pc-windows-msvc', 'format': 1,
                'config_schema': 1, 'minimum_version': policy['minimum_version'],
                'files': {name: len(data) for name, data in sorted(files.items())},
                'remove': policy['remove'], 'excluded_versions': policy['excluded_versions']}
    files['release.json'] = json.dumps(manifest, ensure_ascii=False, indent=2).encode('utf-8')
    return manifest, files


def archive(files):
    output = io.BytesIO()
    with tarfile.open(fileobj=output, mode='w', format=tarfile.USTAR_FORMAT) as tar:
        for name, data in sorted(files.items()):
            info = tarfile.TarInfo(name)
            info.size = len(data)
            info.mode = 0o644
            info.mtime = 0
            tar.addfile(info, io.BytesIO(data))
    return gzip.compress(output.getvalue(), mtime=0)


def write_package(stub, payload, destination, mode):
    checked(destination).write_bytes(stub + payload + struct.pack('<QB', len(payload), mode) + MAGIC)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--build-id', help='Explicitly requested same-version rebuild in a separate archive')
    args = parser.parse_args()
    if os.environ.get('FLARAGON_DEV_ROOT', '').lower() != str(ROOT).lower():
        raise ValueError('Run scripts/env.ps1 first')
    version = tomllib.loads(checked(ROOT / 'Cargo.toml').read_text(encoding='utf-8'))['package']['version']
    if args.build_id:
        if not re.fullmatch(r'[a-z][a-z0-9.-]{0,79}', args.build_id) or not args.build_id.endswith('-' + version):
            raise ValueError('Build identifier must be a safe name ending with the product version')
        output = checked(ROOT / 'dist/rebuilds' / args.build_id)
        if output.exists():
            raise ValueError('Rebuild archive already exists; choose a new build identifier')
    else:
        output = checked(ROOT / 'dist' / version)
    output.mkdir(parents=True, exist_ok=True)
    manifest_path = checked(output / 'release.json')
    previous = json.loads(manifest_path.read_text(encoding='utf-8')) if manifest_path.exists() else None
    release_id = previous['release_id'] if previous else uuid.uuid4().hex
    manifest, files = build_payload(version, release_id)
    stage = checked(output / 'payload')
    if stage.exists():
        existing = {p.relative_to(stage).as_posix() for p in stage.rglob('*') if checked(p).is_file()}
        if not existing.issubset(files):
            raise ValueError('Staging directory contains unknown or obsolete files; choose a new release version')
    for name, data in files.items():
        path = checked(stage / name)
        if previous and (not path.exists() or path.read_bytes() != data):
            raise ValueError('Published version contents differ; use a new product version')
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
    subprocess.run([str(stage / 'Flaragon.exe'), '--deployment-check', version, str(stage)], check=True, timeout=25,
                   creationflags=subprocess.CREATE_NO_WINDOW)
    stub = checked(ROOT / 'target/x86_64-pc-windows-msvc/release/flaragon-deploy.exe').read_bytes()
    payload = archive(files)
    for mode, kind in enumerate(('Setup', 'Update')):
        path = checked(output / f'Flaragon-{kind}-{version}-x64.exe')
        write_package(stub, payload, path, mode)
        print(path)
    checked(output / 'release.json').write_text(json.dumps(manifest, indent=2, ensure_ascii=False) + '\n', encoding='utf-8')
    checked(output / 'build-info.json').write_text(json.dumps({
        'version': version, 'target': manifest['architecture'], 'source': 'Cargo.lock and project release build',
        'components_included': False, 'file_count': len(files), 'signed': False,
        'executable_bytes': len(files['Flaragon.exe']),
        'build_id': args.build_id,
        'release_id': release_id,
    }, indent=2) + '\n', encoding='utf-8')


if __name__ == '__main__':
    main()
