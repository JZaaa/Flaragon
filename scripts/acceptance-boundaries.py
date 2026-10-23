"""Additional isolated behavioral acceptance; uses only owned component instances."""
import copy
import json
from pathlib import Path
import socket
import time
import sys
sys.path.insert(0,str(Path(__file__).resolve().parent))
from acceptance import Session, ROOT, EVIDENCE, http, redis, user_config

results=[]
def passed(name):
    results.append({'name':name,'passed':True})
    print('PASS',name,flush=True)

session=Session()
original=None
redis_original=None
try:
    session.call('scan')
    session.call('stop_all')
    original=copy.deepcopy(session.snapshot['config'])
    config=copy.deepcopy(original)
    config['default_php']=config['php'][0]['package']
    config['projects'][0]['php']=None
    config['projects'][1]['php']=config['php'][0]['package']
    config['projects'][2]['enabled']=False
    for spec in config['mysql']: spec['usage']='disabled'
    config['redis']['usage']='disabled'
    config['php'][2]['usage']='disabled'
    session.call('apply',config=config)
    session.call('start_all')
    assert json.loads(http('accept0.localhost'))['version']=='7.4.33'
    assert all(r['state']=='已停止' for r in session.snapshot['rows'] if r['id'].startswith('mysql:') or r['id']=='redis' or r['id'].endswith('8-5-11'))
    passed('Start all excludes disabled services')
    config=copy.deepcopy(session.snapshot['config']);config['default_php']=config['php'][1]['package']
    session.call('stop_all')
    session.call('apply',config=config)
    session.call('start_all')
    assert json.loads(http('accept0.localhost'))['version']=='8.3.35'
    assert json.loads(http('accept1.localhost'))['version']=='7.4.33'
    passed('Default PHP change affects inherited project only')
    before={r['id']:r['detail'] for r in session.snapshot['rows'] if r['state']=='运行中'}
    # Kill only the child manager created by this script; service processes must survive.
    session.proc.kill();session.proc.wait(timeout=10)
    session=Session();session.call('scan');session.call('verify')
    after={r['id']:r['detail'] for r in session.snapshot['rows'] if r['state']=='运行中'}
    assert before==after
    passed('Manager crash recovery reconnects matching process identities without duplicates')
    session.call('stop_all')
    config=copy.deepcopy(original);config['mysql'][0]['package']='mysql-mysql-8-0-44'
    rejected=session.call('apply',config=config,expect=False)
    assert '不能改名或切换' in rejected['error']
    passed('MySQL refuses cross-version data directory reuse')
    config=copy.deepcopy(original)
    fixture_id='nonempty-'+str(int(time.time()))
    fixture=ROOT/'data/mysql/5.7'/fixture_id;fixture.mkdir(parents=True)
    sentinel=fixture/'user-content.txt';sentinel.write_text('must remain unchanged',encoding='utf-8')
    config['mysql'].append(dict(id=fixture_id,package=config['mysql'][0]['package'],port=13320,usage='normal',name=fixture_id,series='5.7',reuse_data=True))
    rejected=session.call('apply',config=config,expect=False)
    assert '无法确认版本' in rejected['error'] and sentinel.read_text()=='must remain unchanged'
    passed('Nonempty unbound MySQL directory is preserved and initialization refused')
    session.call('apply',config=original)
    with socket.socket() as occupied:
        occupied.bind(('127.0.0.1',16379));occupied.listen()
        rejected=session.call('start',id='redis',expect=False)
        assert '已占用' in rejected['error']
        occupied.settimeout(.2)
        try: occupied.accept();raise AssertionError('Manager connected to unknown listener')
        except TimeoutError:pass
    passed('Redis port conflict rejects startup without contacting external listener')
    redis_original=user_config(session.call('read_config',id='redis')['text'])
    session.call('start',id='redis')
    session.call('override',id='redis',text='appendonly yes\nsave 60 1\nmaxmemory 256mb\n')
    assert '268435456' in redis('CONFIG','GET','maxmemory')
    passed('Redis configuration is verified against live server')
    session.call('override',id='redis',text=redis_original)
    redis_original=None
finally:
    try:
        session.call('stop_all')
        if original is not None:session.call('apply',config=original)
        if redis_original is not None:session.call('override',id='redis',text=redis_original)
    finally:
        session.close()
        (EVIDENCE/'acceptance-boundaries.json').write_text(json.dumps(results,ensure_ascii=False,indent=2),encoding='utf-8')

