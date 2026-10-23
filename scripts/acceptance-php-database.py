"""Verify PHP database drivers and nested HTTP against isolated project services."""
import json
from pathlib import Path
import sys
sys.path.insert(0,str(Path(__file__).resolve().parent))
from acceptance import Session, ROOT, EVIDENCE, http, user_config

s=Session();originals={};results=[]
try:
    s.call('scan');s.call('stop_all')
    config=s.snapshot['config']
    for php in config['php']:
        service_id='php:'+php['package']
        content=user_config(s.call('read_config',id=service_id)['text'])
        originals[service_id]=content
        for extension in ['mysqli','pdo_mysql','mbstring','openssl','curl','fileinfo']:
            # The fixture uses only matching DLLs from each package.
            if not any(line.strip().startswith('extension') and extension in line for line in content.splitlines()):
                content+='\nextension=php_'+extension+'.dll\n'
        s.call('override',id=service_id,text=content)
    s.call('apply',config=config);s.call('start_all')
    for project in config['projects']:
        root=(ROOT/project['directory']/project['document_root']).resolve()
        assert root.is_relative_to(ROOT)
        fixture="""<?php
header('Content-Type: application/json');
$out=['php'=>PHP_VERSION,'drivers'=>PDO::getAvailableDrivers(),'mysql'=>[]];
foreach([13307,13308] as $port){
    $pdo=new PDO('mysql:host=127.0.0.1;port='.$port.';charset=utf8mb4','root','',[PDO::ATTR_ERRMODE=>PDO::ERRMODE_EXCEPTION]);
    $mysqli=new mysqli('127.0.0.1','root','',null,$port);
    $out['mysql'][]=['port'=>$port,'pdo'=>$pdo->query('SELECT VERSION()')->fetchColumn(),'mysqli'=>$mysqli->query('SELECT VERSION()')->fetch_row()[0]];
}
echo json_encode($out);
"""
        (root/'database-check.php').write_text(fixture,encoding='utf-8')
        nested="""<?php
$context=stream_context_create(['http'=>['header'=>'Host: %s\\r\\n','timeout'=>5]]);
echo file_get_contents('http://127.0.0.1:18080/index.php',false,$context);
""" % project['domain']
        # PHP single quoted header must contain actual CRLF for the Host header.
        nested=nested.replace("'header'=>'Host: "+project['domain']+"\\r\\n'",'\'header\'=>"Host: '+project['domain']+'\\r\\n"')
        (root/'nested-check.php').write_text(nested,encoding='utf-8')
        result=json.loads(http(project['domain'],'/database-check.php'))
        assert 'mysql' in result['drivers']
        assert result['mysql'][0]['pdo'].startswith('5.7.44') and result['mysql'][0]['mysqli'].startswith('5.7.44')
        assert result['mysql'][1]['pdo'].startswith('8.0.44') and result['mysql'][1]['mysqli'].startswith('8.0.44')
        assert json.loads(http(project['domain'],'/nested-check.php'))['version']==result['php']
        results.append({'php':result['php'],'mysql':result['mysql'],'nested_http':True,'passed':True})
        print('PASS',result['php'],'PDO + mysqli -> MySQL 5.7/8.0; nested HTTP',flush=True)
finally:
    try:
        s.call('stop_all')
        for service_id,content in originals.items():s.call('override',id=service_id,text=content)
        s.call('apply',config=s.snapshot['config'])
    finally:
        s.close()
        (EVIDENCE/'acceptance-php-database.json').write_text(json.dumps(results,ensure_ascii=False,indent=2),encoding='utf-8')
