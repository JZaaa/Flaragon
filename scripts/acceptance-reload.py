"""Verify Apache reload using isolated project services and actual HTTP responses."""
import json
from http.client import HTTPConnection
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parent))
from acceptance import Session, ROOT, EVIDENCE

results = []
session = Session()
override = None
original = None

def passed(name):
    results.append(dict(name=name, passed=True))
    print('PASS', name, flush=True)

def identities():
    state = json.loads((ROOT / 'runtime/state/processes.json').read_text(encoding='utf-8'))
    return {r['id'] + ':' + str(r['identity']['pid']): r['identity'] for r in state['records']}

def keepalive():
    site = session.snapshot['config']['projects'][0]
    path = ROOT / site['directory'] / site['document_root'] / 'reload-check.txt'
    path.write_text('reload check', encoding='utf-8')
    connection = HTTPConnection('127.0.0.1', session.snapshot['config']['http_port'], timeout=10)
    connection.request('GET', '/reload-check.txt', headers={'Host': site['domain'], 'Connection': 'keep-alive'})
    response = connection.getresponse()
    assert response.status == 200 and response.read() == b'reload check'
    header = response.getheader('Keep-Alive', '')
    connection.close()
    return header

try:
    session.call('scan')
    session.call('start_all')
    native = session.call('read_config', id='apache')
    override = Path(native['path'])
    original = native['text']
    before = identities()
    def configured(extra):
        return original.replace('# BEGIN FLARAGON PROJECTS', extra + '\n# BEGIN FLARAGON PROJECTS')
    override.write_text(configured('KeepAliveTimeout 7'), encoding='utf-8')
    session.call('reload', id='apache')
    assert identities() == before
    assert 'timeout=7' in keepalive(), keepalive()
    session.call('verify')
    passed('Apache reload applies KeepAliveTimeout to actual HTTP; parent and all other service identities unchanged')
    override.write_text(configured('KeepAliveTimeout 9'), encoding='utf-8')
    session.call('reload', id='apache')
    assert 'timeout=9' in keepalive()
    assert identities() == before
    passed('Repeated reload applies new settings without duplicating managed processes')
    state = json.loads((ROOT / 'runtime/state/processes.json').read_text(encoding='utf-8'))
    active = ROOT / next(r['config'] for r in state['records'] if r['id'] == 'apache')
    active_bytes = Path(active).read_bytes()
    override.write_text(configured('KeepAlive invalid-value'), encoding='utf-8')
    session.call('reload', id='apache', expect=False)
    assert Path(active).read_bytes() == active_bytes
    assert 'timeout=9' in keepalive() and identities() == before
    passed('Invalid reload restores the native file and leaves the live service unchanged')
    for row in session.snapshot['rows']:
        if row['id'] != 'apache':
            session.call('reload', id=row['id'], expect=False)
    assert identities() == before
    passed('Services without full configuration reload reject it without side effects')
finally:
    try:
        if original is not None:
            override.write_text(original, encoding='utf-8')
            session.call('reload', id='apache')
        session.call('stop_all')
    finally:
        session.close()
        (EVIDENCE / 'acceptance-reload.json').write_text(json.dumps(results, ensure_ascii=False, indent=2), encoding='utf-8')
