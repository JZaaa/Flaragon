"""Collect license and copyright notices from the project-local locked dependency cache."""
from pathlib import Path
import json
import shutil
import tomllib
import subprocess

ROOT = Path(__file__).resolve().parent.parent
CACHE = ROOT / '.cache/cargo/registry/src'
OUTPUT = ROOT / 'licenses/rust'


def checked(path):
    result = path.resolve()
    if not result.is_relative_to(ROOT):
        raise ValueError(f'Path outside workspace: {path}')
    if path.is_symlink():
        raise ValueError(f'Linked path: {path}')
    return result


def main():
    OUTPUT.mkdir(parents=True, exist_ok=True)
    packages = tomllib.loads((ROOT / 'Cargo.lock').read_text(encoding='utf-8'))['package']
    records = []
    previous_file = OUTPUT / 'index.json'
    previous = {(p['name'], p['version']): p for p in json.loads(previous_file.read_text(encoding='utf-8'))} if previous_file.exists() else {}
    tree = subprocess.check_output(['cargo', 'tree', '--locked', '--offline', '--target', 'x86_64-pc-windows-msvc', '--edges', 'normal,build', '--prefix', 'none', '--format', '{p}'], cwd=ROOT, text=True)
    active = {(line.split()[0], line.split()[1][1:]) for line in tree.splitlines()}
    for package in packages:
        if not package.get('source', '').startswith('registry+'):
            continue
        folder = f"{package['name']}-{package['version']}"
        sources = list(CACHE.glob(f'*/{folder}'))
        if len(sources) != 1:
            raise ValueError(f'Expected cached crate {folder}')
        source = checked(sources[0])
        manifest = tomllib.loads((source / 'Cargo.toml').read_text(encoding='utf-8'))['package']
        files = []
        for path in source.rglob('*'):
            checked(path)
            if not path.is_file():
                continue
            relative = path.relative_to(source)
            if any(part.startswith('.') for part in relative.parts):
                continue
            if not (path.name.lower().startswith(('license', 'licence', 'copying', 'copyright', 'notice')) or relative.parts[0].lower() in ('licenses', 'license')):
                continue
            destination = checked(OUTPUT / folder / relative)
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(path, destination)
            files.append(relative.as_posix())
        prior = previous.get((package['name'], package['version']), {})
        sources_map = prior.get('notice_sources', {})
        for name in sources_map:
            if checked(OUTPUT / folder / name).is_file() and name not in files:
                files.append(name)
        records.append(dict(name=package['name'], version=package['version'], license=manifest.get('license'), repository=manifest.get('repository'), notices=files, notice_sources=sources_map, active=(package['name'], package['version']) in active))
    (OUTPUT / 'index.json').write_text(json.dumps(records, ensure_ascii=False, indent=2), encoding='utf-8')
    missing = [p['name'] for p in records if p['active'] and not p['notices']]
    print(f'{len(records)} crates; active crates missing notice files: {missing}')
    if missing:
        raise ValueError('Missing notices for active build dependencies')


if __name__ == '__main__':
    main()
