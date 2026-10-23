"""Validate real deployment packages in independent workspace-only installations."""
import ctypes
from ctypes import wintypes
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import time
import tomllib
import uuid

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location('package_builder', ROOT / 'scripts/package.py')
builder = importlib.util.module_from_spec(spec)
spec.loader.exec_module(builder)
check = builder.checked
RESULTS = []


def record(name):
    RESULTS.append(name)
    print(f'PASS {name}', flush=True)


def run(exe, action, target, ok=True):
    result = subprocess.run([str(check(exe)), action, str(check(target))], capture_output=True,
                            encoding='utf-8', timeout=90, creationflags=subprocess.CREATE_NO_WINDOW)
    value = json.loads(result.stdout.strip())
    assert value['ok'] == ok and (result.returncode == 0) == ok, (result.returncode, value, result.stderr)
    return value


def write(root, name, data):
    path = check(root / name)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)


def program(root):
    manifest = json.loads(check(root / 'release.json').read_text(encoding='utf-8'))
    return {name: check(root / name).read_bytes() for name in [*manifest['files'], 'release.json', '.flaragon-install.json']}


def child_identity(child, executable):
    kernel = ctypes.WinDLL('kernel32', use_last_error=True)
    kernel.GetProcessTimes.argtypes = [wintypes.HANDLE] + [ctypes.POINTER(wintypes.FILETIME)] * 4
    kernel.GetProcessTimes.restype = wintypes.BOOL
    times = [wintypes.FILETIME() for _ in range(4)]
    assert kernel.GetProcessTimes(int(child._handle), *(ctypes.byref(v) for v in times))
    return {'pid': child.pid, 'created': times[0].dwHighDateTime << 32 | times[0].dwLowDateTime,
            'executable': str(executable)}


def main():
    assert os.environ.get('FLARAGON_DEV_ROOT', '').lower() == str(ROOT).lower()
    current = tomllib.loads(check(ROOT / 'Cargo.toml').read_text(encoding='utf-8'))['package']['version']
    assert current != '0.2.0', 'Build the baseline 0.2.0 and then the current release first'
    base = check(ROOT / 'testwork/deployment' / uuid.uuid4().hex)
    base.mkdir(parents=True)
    old_setup = check(ROOT / 'dist/0.2.0/Flaragon-Setup-0.2.0-x64.exe')
    old_update = check(ROOT / 'dist/0.2.0/Flaragon-Update-0.2.0-x64.exe')
    setup = check(ROOT / f'dist/{current}/Flaragon-Setup-{current}-x64.exe')
    update = check(ROOT / f'dist/{current}/Flaragon-Update-{current}-x64.exe')
    cli = check(ROOT / 'target/x86_64-pc-windows-msvc/release/flaragon-cli.exe')
    a, b, fresh = (base / n for n in ['软件 A 中文 空格', '软件 B', '新安装'])
    for folder in [a, b]:
        run(old_setup, '--apply', folder)
    run(setup, '--apply', fresh)
    assert not (fresh / 'config').exists() and not (fresh / 'data').exists()
    assert not any((fresh / 'bin/php').iterdir())
    record('real_setup_in_unicode_path_software_only')
    unknown = base / '只有同名程序'
    write(unknown, 'Flaragon.exe', (fresh / 'Flaragon.exe').read_bytes())
    assert '安装身份' in run(update, '--inspect', unknown, False)['error']
    run(setup, '--apply', unknown, False)
    run(update, '--inspect', ROOT, False)
    record('same_name_executable_nonempty_and_source_directory_rejected')
    before_b = program(b)
    config = {'schema': 1, 'revision': 7, 'http_port': 8080, 'apache': None, 'default_php': None,
              'php': [], 'mysql': [], 'redis': None, 'projects': []}
    protected = {'config/app.json': json.dumps(config).encode(),
                 'config/startup.json': b'{"launch_at_login": false, "start_common_services": true}',
                 'data/mysql/sentinel': b'new test data', 'www/site/index.php': b'project sentinel',
                 'bin/php/sentinel.txt': b'user component placeholder', 'runtime/temp/session': b'session sentinel'}
    for name, data in protected.items():
        write(a, name, data)
    before_identity = (a / '.flaragon-install.json').read_bytes()
    # The other environment stays alive during A's update. EOF exits this helper normally.
    other = subprocess.Popen([str(cli), str(b)], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                             stderr=subprocess.PIPE, creationflags=subprocess.CREATE_NO_WINDOW)
    try:
        other.stdin.write(b'{"action":"snapshot"}\n'); other.stdin.flush()
        assert json.loads(other.stdout.readline())['ok']
        run(update, '--inspect', a)
        run(update, '--apply', a)
        assert other.poll() is None
    finally:
        other.stdin.close(); other.wait(timeout=20)
        assert other.returncode == 0
    assert program(b) == before_b
    assert (a / '.flaragon-install.json').read_bytes() == before_identity
    for name, data in protected.items():
        assert (a / name).read_bytes() == data
    record('real_version_upgrade_preserves_data_identity_and_other_running_environment')
    unchanged = program(a)
    run(update, '--apply', a, False)
    run(old_update, '--apply', a, False)
    run(update, '--recover', a, False)
    assert program(a) == unchanged
    record('same_version_downgrade_and_rollback_command_rejected')
    # A managed-process identity fixture runs only the CLI, in its own independent environment.
    helper = check(b / 'bin/identity-helper.exe')
    shutil.copyfile(cli, helper)
    child = subprocess.Popen([str(helper), str(base / '进程身份夹具')], stdin=subprocess.PIPE,
                             stdout=subprocess.PIPE, stderr=subprocess.PIPE, creationflags=subprocess.CREATE_NO_WINDOW)
    try:
        child.stdin.write(b'{"action":"snapshot"}\n'); child.stdin.flush()
        assert json.loads(child.stdout.readline())['ok']
        state = b / 'runtime/state/processes.json'
        original_state = check(state).read_bytes() if state.exists() else None
        state.write_text(json.dumps({'records': [{'identity': child_identity(child, helper)}]}), encoding='utf-8')
        error = run(update, '--apply', b, False)['error']
        assert '受管服务尚未退出' in error
        assert program(b) == before_b and child.poll() is None
    finally:
        child.stdin.close(); child.wait(timeout=20)
        assert child.returncode == 0
        if original_state is None:
            check(state).unlink()
        else:
            state.write_bytes(original_state)
    record('recorded_live_process_blocks_update_without_stopping_it')
    # Recreate an interrupted transaction with exact published new files; no old-version backups.
    next_manifest = json.loads((ROOT / f'dist/{current}/release.json').read_text(encoding='utf-8'))
    token = uuid.uuid4().hex
    journal = {'format': 1, 'token': token, 'mode': 'Update',
               'installation': json.loads((b / '.flaragon-install.json').read_text(encoding='utf-8')),
               'previous': json.loads((b / 'release.json').read_text(encoding='utf-8')), 'next': next_manifest}
    for name in [*next_manifest['files'], 'release.json']:
        write(b, f'.maintenance/{token}/new/{name}', check(ROOT / f'dist/{current}/payload' / name).read_bytes())
    write(b, '.maintenance/active.json', json.dumps(journal).encode())
    write(b, 'Flaragon.exe', b'interrupted fixture')
    blocked = subprocess.run([str(cli), str(b)], capture_output=True, timeout=20, creationflags=subprocess.CREATE_NO_WINDOW)
    assert blocked.returncode != 0
    run(old_update, '--apply', b, False)
    run(update, '--apply', b)
    assert not (b / '.maintenance/active.json').exists()
    assert json.loads((b / 'release.json').read_text())['version'] == current
    record('interrupted_update_blocks_start_and_repairs_only_forward')
    broken = base / '自检失败夹具'
    run(old_setup, '--apply', broken)
    previous_exe = (broken / 'Flaragon.exe').read_bytes()
    files = {name: check(ROOT / f'dist/{current}/payload' / name).read_bytes() for name in next_manifest['files']}
    bad_manifest = dict(next_manifest, version='9.0.0', release_id=uuid.uuid4().hex)
    files['release.json'] = json.dumps(bad_manifest).encode()
    bad_package = base / 'test-only-wrong-version.exe'
    builder.write_package(check(ROOT / 'target/x86_64-pc-windows-msvc/release/flaragon-deploy.exe').read_bytes(),
                          builder.archive(files), bad_package, 1)
    error = run(bad_package, '--apply', broken, False)['error']
    assert '自检失败' in error and (broken / '.maintenance/active.json').exists()
    assert (broken / 'Flaragon.exe').read_bytes() != previous_exe
    record('real_self_check_failure_leaves_pending_without_rollback')
    evidence = {'version': current, 'baseline': '0.2.0', 'source': 'project release binaries and embedded packages',
                'fixture_root': str(base), 'checks': RESULTS, 'children_exited': True,
                'limits': ['Managed process fixture uses the CLI, not database compatibility testing',
                           'Cross-machine, real disk exhaustion and power-loss hardware faults not tested']}
    write(ROOT, 'testwork/evidence/deployment.json', json.dumps(evidence, ensure_ascii=False, indent=2).encode())
    print(json.dumps({'checks': len(RESULTS), 'fixture_root': str(base)}, ensure_ascii=False))


if __name__ == '__main__':
    main()
