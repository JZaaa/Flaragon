"""Validate initial-release packages using new, project-local installations."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tomllib
import uuid

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location('package_builder', ROOT / 'scripts/package.py')
builder = importlib.util.module_from_spec(spec)
spec.loader.exec_module(builder)
checked = builder.checked


def main():
    assert os.environ.get('FLARAGON_DEV_ROOT', '').lower() == str(ROOT).lower()
    version = tomllib.loads(checked(ROOT / 'Cargo.toml').read_text(encoding='utf-8'))['package']['version']
    release = checked(ROOT / 'dist' / version)
    manifest = json.loads(checked(release / 'release.json').read_text(encoding='utf-8'))
    assert manifest['version'] == manifest['minimum_version'] == version
    assert manifest['architecture'] == 'x86_64-pc-windows-msvc'
    expected = {'Flaragon.exe', 'lib/Scintilla.dll', 'lib/Lexilla.dll'}
    assert expected.issubset(manifest['files'])
    assert all(name in expected or name.startswith('licenses/') for name in manifest['files'])
    base = checked(ROOT / 'testwork' / 'initial-release' / uuid.uuid4().hex)
    base.mkdir(parents=True)
    setup = checked(release / f'Flaragon-Setup-{version}-x64.exe')
    update = checked(release / f'Flaragon-Update-{version}-x64.exe')
    cli = checked(ROOT / 'target/x86_64-pc-windows-msvc/release/flaragon-cli.exe')
    results = []

    def record(name):
        results.append(name)
        print(f'PASS {name}', flush=True)

    def deploy(exe, action, target, ok=True):
        result = subprocess.run([str(checked(exe)), action, str(checked(target))],
                                capture_output=True, encoding='utf-8', timeout=90,
                                creationflags=subprocess.CREATE_NO_WINDOW)
        response = json.loads(result.stdout.strip())
        assert response['ok'] == ok and (result.returncode == 0) == ok, response
        return response

    def program(folder):
        names = [*manifest['files'], 'release.json', '.flaragon-install.json']
        return {name: checked(folder / name).read_bytes() for name in names}

    record('initial_version_and_software_only_manifest')
    first, second = base / '初始安装 中文 空格', base / 'independent install'
    deploy(setup, '--inspect', first)
    assert not first.exists()
    deploy(setup, '--apply', first)
    installed = json.loads(checked(first / 'release.json').read_text(encoding='utf-8'))
    assert installed == manifest
    for name, size in manifest['files'].items():
        content = checked(first / name).read_bytes()
        assert len(content) == size
        assert content == checked(release / 'payload' / name).read_bytes()
    for component in ('apache', 'php', 'mysql', 'redis'):
        assert not any(checked(first / 'bin' / component).iterdir())
    assert not checked(first / 'config').exists() and not checked(first / 'data').exists()
    record('unicode_space_installation_matches_payload_without_user_data')

    subprocess.run([str(checked(first / 'Flaragon.exe')), '--deployment-check', version, str(first)],
                   check=True, timeout=25, creationflags=subprocess.CREATE_NO_WINDOW)
    wrong = subprocess.run([str(checked(first / 'Flaragon.exe')), '--deployment-check', 'invalid', str(first)],
                           timeout=25, creationflags=subprocess.CREATE_NO_WINDOW)
    assert wrong.returncode != 0
    record('installed_executable_and_editor_dependencies_pass_version_self_check')

    deploy(setup, '--apply', second)
    assert checked(first / '.flaragon-install.json').read_bytes() != checked(second / '.flaragon-install.json').read_bytes()
    other_before = program(second)
    response = subprocess.run([str(cli), str(first)], input='{"action":"snapshot"}\n',
                              capture_output=True, encoding='utf-8', timeout=30,
                              creationflags=subprocess.CREATE_NO_WINDOW)
    assert response.returncode == 0, response.stderr
    assert json.loads(response.stdout.strip())['ok']
    assert program(second) == other_before
    record('independent_installations_and_empty_environment_initialization')

    before = program(first)
    deploy(update, '--inspect', first, False)
    deploy(update, '--apply', first, False)
    deploy(setup, '--apply', first, False)
    assert program(first) == before
    record('same_version_update_and_reinstallation_rejected_without_replacement')

    unknown = checked(base / 'unknown target')
    unknown.mkdir()
    sentinel = checked(unknown / 'keep.txt')
    sentinel.write_text('independent test fixture', encoding='utf-8')
    deploy(setup, '--apply', unknown, False)
    deploy(update, '--inspect', unknown, False)
    deploy(setup, '--inspect', ROOT, False)
    deploy(update, '--inspect', ROOT, False)
    assert sentinel.read_text(encoding='utf-8') == 'independent test fixture'
    assert not checked(unknown / 'Flaragon.exe').exists()
    record('unknown_nonempty_and_source_directories_rejected')

    evidence = checked(ROOT / f'testwork/evidence/initial-release-{version}.json')
    evidence.parent.mkdir(parents=True, exist_ok=True)
    evidence.write_text(json.dumps({
        'version': version, 'source': 'real packaged EXEs and exact payload byte comparisons',
        'fixture_root': str(base), 'checks': results, 'children_exited': True,
        'limits': ['No earlier compatible release exists; cross-version upgrades are not tested',
                   'No services started; this check does not replace component or GUI validation',
                   'Cross-machine, Windows 10 and DPI matrices are not tested'],
    }, ensure_ascii=False, indent=2), encoding='utf-8')
    print(json.dumps({'version': version, 'checks': len(results), 'evidence': str(evidence)}, ensure_ascii=False))


if __name__ == '__main__':
    main()
