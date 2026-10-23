"""Exercise directory adoption using newly initialized, owned MySQL data only."""
import copy
import importlib.util
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess

spec = importlib.util.spec_from_file_location('web', Path(__file__).with_name('acceptance-web-entries.py'))
web = importlib.util.module_from_spec(spec); spec.loader.exec_module(web)
checked = web.checked

def copy_tree(source, target, ignore=None):
    source, target = checked(source), checked(target)
    for directory, dirs, files in os.walk(source, followlinks=False):
        checked(directory)
        for name in dirs + files: checked(Path(directory) / name)
    target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copytree(source, target, ignore=ignore)

def free_ports(count):
    sockets = []
    try:
        for _ in range(count):
            s=socket.socket(); sockets.append(s); s.bind(('127.0.0.1',0))
        return [s.getsockname()[1] for s in sockets]
    finally:
        for s in sockets: s.close()

def main():
    checked(web.ROOT).mkdir(parents=True)
    for name in ['mysql-5.7.44', 'mysql-8.0.44']:
        copy_tree(web.WORKSPACE / 'bin/mysql' / name, web.ROOT / 'bin/mysql' / name)
    session=web.Session(); results=[]
    def call(action, **kwargs): return session.call(action, **kwargs)
    def directory(db): return checked(web.ROOT / 'data/mysql' / db['series'] / db['name'])
    def sql(db, statement):
        assert any(r['id']=='mysql:'+db['id'] and r['running'] for r in session.snapshot['rows'])
        package=packages[db['package']]
        exe=checked(Path(package['executable']).with_name('mysql.exe'))
        env={'SystemRoot':os.environ.get('SystemRoot',r'C:\Windows'), 'FLARAGON_DEV_ROOT':str(web.WORKSPACE),
             'TEMP':str(web.ROOT/'runtime/temp'),'TMP':str(web.ROOT/'runtime/temp'),
             'HOME':str(web.ROOT), 'USERPROFILE':str(web.ROOT), 'MYSQL_HISTFILE':str(web.ROOT/'runtime/temp/mysql-history')}
        proc=subprocess.run([str(exe),'--no-defaults','--protocol=TCP','-h127.0.0.1',f'-P{db["port"]}','-uroot','-N','-B','-e',statement],
            env=env,cwd=web.ROOT,capture_output=True,encoding='utf-8',timeout=30,creationflags=subprocess.CREATE_NO_WINDOW)
        assert proc.returncode==0,proc.stderr
        return proc.stdout.strip()
    def review(db): return call('mysql_adoption_review',id=db['id'])['review']
    def confirm(r, **changes):
        return dict(token=r['token'],source_version=r['version'],source_stopped=True,
            backup_confirmed=True,config_confirmed=True,**changes)
    try:
        call('scan'); packages={p['id']:p for p in session.snapshot['packages']}
        assert all(p['error'] is None for p in packages.values()), packages
        config=copy.deepcopy(session.snapshot['config']); config['mysql']=[]
        ports=iter(free_ports(8))
        config['http_port']=next(ports)
        for p in packages.values():
            series='.'.join(p['version'].split('.')[:2])
            for suffix in ['source','imported','failed']:
                config['mysql'].append(dict(id=series.replace('.','-')+'-'+suffix,package=p['id'],series=series,
                    name=suffix,port=next(ports),usage='normal',reuse_data=False))
        call('settings',config=config)
        for p in packages.values():
            dbs=[db for db in config['mysql'] if db['package']==p['id']]
            source,target,failed=dbs
            assert call('mysql_directory',id=target['id'])['report']['state']=='missing'
            call('start',id='mysql:'+source['id'])
            assert sql(source,"CREATE DATABASE adoption_check; CREATE TABLE adoption_check.items (id INT PRIMARY KEY, value VARCHAR(50)); INSERT INTO adoption_check.items VALUES (1,'kept'); SET GLOBAL innodb_fast_shutdown=0;")==''
            call('mysql_adoption_review',expect=False,id=source['id'])
            call('stop',id='mysql:'+source['id'])
            assert not checked(directory(source)/'.flaragon-initializing.json').exists()
            for dest in [target,failed]:
                copy_tree(directory(source),directory(dest),shutil.ignore_patterns('.flaragon-binding.json'))
            report=call('mysql_directory',id=target['id'])['report']
            assert report['state']=='unbound' and report['can_adopt'],report
            call('start',expect=False,id='mysql:'+target['id'])
            assert not checked(directory(target)/'.flaragon-binding.json').exists()
            r=review(target); bad=confirm(r); bad['backup_confirmed']=False
            call('mysql_adopt',expect=False,confirmation=bad)
            r=review(target); bad=confirm(r); bad['source_version']='5.7.1'
            call('mysql_adopt',expect=False,confirmation=bad)
            r=review(target)
            checked(directory(target)/'concurrent-note.txt').write_text('metadata edit',encoding='utf-8')
            call('mysql_adopt',expect=False,confirmation=confirm(r))
            assert not checked(directory(target)/'.flaragon-binding.json').exists()
            r=review(target)
            with socket.socket() as occupied:
                occupied.bind(('127.0.0.1',target['port'])); occupied.listen()
                call('mysql_adopt',expect=False,confirmation=confirm(r))
            assert not checked(directory(target)/'.flaragon-binding.json').exists()
            r=review(target); call('mysql_adopt',confirmation=confirm(r))
            assert not any(row['running'] for row in session.snapshot['rows'])
            binding=json.loads(checked(directory(target)/'.flaragon-binding.json').read_bytes())
            assert binding['origin']=='adopted' and binding['version']==p['version']
            assert call('mysql_directory',id=target['id'])['report']['state']=='bound'
            call('mysql_adoption_review',expect=False,id=target['id'])
            for _ in range(2):
                call('start',id='mysql:'+target['id'])
                assert sql(target,'SELECT value FROM adoption_check.items WHERE id=1')=='kept'
                call('stop',id='mysql:'+target['id'])
            # A page size inconsistent with this fresh fixture must fail without initializing it.
            result=call('read_config',id='mysql:'+failed['id'])
            text=result['text']+'\n[mysqld]\ninnodb_page_size=4096\n'
            call('save_config',id='mysql:'+failed['id'],text=text)
            r=review(failed)
            call('mysql_adopt',expect=False,confirmation=confirm(r))
            assert not checked(directory(failed)/'.flaragon-binding.json').exists()
            assert checked(directory(failed)/'ibdata1').exists()
            assert call('mysql_directory',id=failed['id'])['report']['state']=='adoption_pending'
            call('save_config',id='mysql:'+failed['id'],text=result['text'])
            r=review(failed); call('mysql_adopt',confirmation=confirm(r))
            call('start',id='mysql:'+failed['id'])
            assert sql(failed,'SELECT value FROM adoption_check.items WHERE id=1')=='kept'
            call('stop',id='mysql:'+failed['id'])
            results.append(dict(version=p['version'],checks=['new initialization','unbound refusal','confirmation and source version',
                'concurrent edit rejection','port conflict rejection','adoption and normal stop','data retained across restart',
                'startup failure preserves data without binding','explicit retry after failure']))
            print('PASS',p['version'],flush=True)
    finally:
        try: call('stop_all')
        finally: session.process.stdin.close(); session.process.wait(timeout=20)
    web.write(web.WORKSPACE/'testwork/evidence/mysql-adoption.json',json.dumps(dict(environment=str(web.ROOT),
        executable=str(web.EXE),results=results,stopped=True),ensure_ascii=False,indent=2))
    print('ENVIRONMENT',web.ROOT)

if __name__=='__main__': main()
