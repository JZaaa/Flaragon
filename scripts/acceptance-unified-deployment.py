"""Build an isolated development package and validate archived upgrade sources automatically."""
import importlib.util
import argparse
from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import subprocess
import sys
import tomllib
import uuid

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location('builder', ROOT / 'scripts/package.py')
builder = importlib.util.module_from_spec(spec)
spec.loader.exec_module(builder)
checked = builder.checked


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--release', type=Path, help='Validate an existing unified candidate without rebuilding it')
    args = parser.parse_args()
    assert os.environ.get('FLARAGON_DEV_ROOT', '').lower() == str(ROOT).lower()
    version = tomllib.loads(checked(ROOT / 'Cargo.toml').read_text(encoding='utf-8'))['package']['version']
    evidence_root = checked(ROOT / 'testwork' / ('unified-' + uuid.uuid4().hex[:12]))
    evidence_root.mkdir(parents=True)
    output = checked(args.release.absolute()) if args.release else evidence_root
    if args.release:
        manifest = json.loads(checked(output / 'release.json').read_bytes())
        assert manifest['version'] == version
    else:
        manifest, files = builder.build_payload(version, uuid.uuid4().hex)
        for name, data in files.items():
            path = checked(output / 'payload' / name)
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
        checked(output / 'release.json').write_bytes(files['release.json'])
        stub = checked(ROOT / 'target/x86_64-pc-windows-msvc/release/flaragon-deploy.exe').read_bytes()
    package = checked(output / f'Flaragon-Setup-{version}-x64.exe')
    if not args.release:
        builder.write_package(stub, builder.archive(files), package, 3)
    assert package.read_bytes()[-17] == 3
    sources = [ROOT / 'dist/rebuilds/latest-dev-0.0.2', ROOT / 'dist/archives/original-0.0.4', ROOT / 'dist/0.0.4']
    bound_path = checked(output / 'update-base.json')
    if bound_path.exists():
        bound = json.loads(bound_path.read_bytes())
        matches = []
        for candidate in [ROOT / 'dist' / version, ROOT / 'dist/archives' / ('original-' + version)]:
            baseline = checked(candidate / 'release.json')
            if baseline.exists() and json.loads(baseline.read_bytes()) == bound:
                matches.append(candidate)
        assert matches, 'The exact bound same-version archive is required'
        sources.append(matches[0])
    def validate_source(source):
        source = checked(source)
        if not (source / 'release.json').exists():
            return {'source': str(source), 'status': 'unavailable'}
        baseline = json.loads(checked(source / 'release.json').read_bytes())['version']
        print(f'Validating {source.name} -> {version}', flush=True)
        run = subprocess.run([sys.executable, str(ROOT / 'scripts/acceptance-release.py'),
            '--baseline', baseline, '--baseline-release', str(source), '--release', str(output)],
            capture_output=True, encoding='utf-8', timeout=600, creationflags=subprocess.CREATE_NO_WINDOW)
        checked(evidence_root / f'{source.name}.log').write_text(run.stdout + run.stderr, encoding='utf-8')
        if run.returncode:
            raise RuntimeError(run.stdout + run.stderr)
        print(f'Passed {source.name} -> {version}', flush=True)
        return {'source': str(source), 'version': baseline,
                'release_id': json.loads((source / 'release.json').read_bytes())['release_id'], 'status': 'passed'}
    with ThreadPoolExecutor(max_workers=2) as executor:
        results = list(executor.map(validate_source, sources))
    same = checked(ROOT / 'dist' / version / f'Flaragon-Setup-{version}-x64.exe')
    if same.exists() and not bound_path.exists():
        target = checked(evidence_root / 'same-version')
        subprocess.run([str(same), '--apply', str(target)], check=True, capture_output=True, timeout=90,
            creationflags=subprocess.CREATE_NO_WINDOW)
        before = {name: ((target / name).read_bytes(), (target / name).stat().st_mtime_ns)
                  for name in [*json.loads((target / 'release.json').read_bytes())['files'], 'release.json']}
        for action in ['--inspect', '--apply']:
            run = subprocess.run([str(package), action, str(target)], capture_output=True, encoding='utf-8',
                timeout=90, creationflags=subprocess.CREATE_NO_WINDOW)
            assert run.returncode and not json.loads(run.stdout)['ok'], run.stdout
        assert all(((target / name).read_bytes(), (target / name).stat().st_mtime_ns) == value
                   for name, value in before.items())
        results.append({'source': str(same), 'status': 'same-version rejected without writes'})
    evidence = {'package': str(package), 'release_id': manifest['release_id'], 'development_only': not bool(args.release),
                'matrix': results, 'limits': ['No archived original 0.0.1 or 0.0.3 installer available',
                    'Database files are new sentinels; no database migration or service startup performed']}
    checked(evidence_root / 'results.json').write_text(json.dumps(evidence, ensure_ascii=False, indent=2), encoding='utf-8')
    print('Evidence: ' + str(evidence_root / 'results.json'), flush=True)
    print(json.dumps(evidence, ensure_ascii=False, indent=2), flush=True)


if __name__ == '__main__':
    main()
