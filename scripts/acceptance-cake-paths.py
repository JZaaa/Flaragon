"""Exercise real CakePHP request, routing, URL, session and upload components under Apache paths.

Requires workspace-local Composer installs at testwork/cake-framework/cake4 and cake5.
Their composer.lock files identify the exact framework and dependencies; no application data is reused.
"""
import copy
import importlib.util
import json
import os
from pathlib import Path
import shutil
import sys

spec = importlib.util.spec_from_file_location('web_acceptance', Path(__file__).with_name('acceptance-web-entries.py'))
web = importlib.util.module_from_spec(spec)
spec.loader.exec_module(web)
web.EXE = web.WORKSPACE / 'target/x86_64-pc-windows-msvc/release/flaragon-cli.exe'
web.ROOT = web.WORKSPACE / 'testwork' / ('cake-paths-' + web.ROOT.name.removeprefix('web-entries-'))
SITE_ROOT = web.ROOT.with_name(web.ROOT.name + '-sites') if web.EXTERNAL_SITES else web.ROOT


def copy_checked(source, destination):
    source = web.checked(source)
    for directory, folders, files in os.walk(source, followlinks=False):
        web.checked(directory)
        for name in folders + files: web.checked(Path(directory) / name)
    shutil.copytree(source, web.checked(destination))


def main():
    web.checked(web.ROOT).mkdir(parents=True)
    for component in ['apache', 'php']:
        copy_checked(web.WORKSPACE / 'bin' / component, web.ROOT / 'bin' / component)
    frameworks = []
    for major in ['4', '5']:
        source = web.WORKSPACE / 'testwork/cake-framework' / ('cake' + major)
        copy_checked(source, SITE_ROOT / 'www' / ('cake' + major))
        lock = json.loads((source / 'composer.lock').read_text(encoding='utf-8'))
        frameworks.extend({'package': p['name'], 'version': p['version'], 'source': p.get('source', {})} for p in lock['packages'] if p['name'] == 'cakephp/cakephp')
        web.write(SITE_ROOT / 'www' / ('cake' + major) / '.htaccess', 'RewriteEngine On\nRewriteRule ^$ webroot/ [L]\nRewriteRule (.*) webroot/$1 [L]\n')
        web.write(SITE_ROOT / 'www' / ('cake' + major) / 'webroot/.htaccess', 'RewriteEngine On\nRewriteCond %{REQUEST_FILENAME} !-f\nRewriteRule ^ index.php [L]\n')
        web.write(SITE_ROOT / 'www' / ('cake' + major) / 'webroot/css/app.css', 'body { color: #123; }')
        web.write(SITE_ROOT / 'www' / ('cake' + major) / 'webroot/index.php', r'''<?php
require dirname(__DIR__) . '/vendor/autoload.php';
use Cake\Core\Configure;
use Cake\Http\ServerRequestFactory;
use Cake\Routing\Router;
use Cake\View\View;
use Cake\View\Helper\HtmlHelper;
Configure::write('App', ['namespace'=>'App', 'encoding'=>'UTF-8', 'webroot'=>'webroot', 'dir'=>'src', 'base'=>false, 'baseUrl'=>false]);
$request = ServerRequestFactory::fromGlobals();
Router::reload();
Router::setRequest($request);
$routes = Router::createRouteBuilder('/');
$routes->scope('/', function ($routes) {
    $routes->connect('/users/view/{id}', ['controller'=>'Users', 'action'=>'view']);
    $routes->connect('/login', ['controller'=>'Users', 'action'=>'login']);
    $routes->connect('/upload', ['controller'=>'Users', 'action'=>'upload']);
});
$params = Router::parseRequest($request);
$session = $request->getSession();
$session->write('visits', (int)$session->read('visits') + 1);
$html = new HtmlHelper(new View($request));
$files = [];
foreach ($request->getUploadedFiles() as $key => $file) { $files[$key] = [$file->getClientFilename(), (string)$file->getStream()]; }
if ($request->getQuery('redirect')) { header('Location: '.Router::url(['controller'=>'Users','action'=>'login'])); exit; }
header('Content-Type: application/json');
echo json_encode(['php'=>PHP_VERSION, 'base'=>$request->getAttribute('base'), 'webroot'=>$request->getAttribute('webroot'), 'params'=>$params,
    'url'=>Router::url(['controller'=>'Users','action'=>'view','id'=>'42']), 'css'=>$html->css('app'), 'visits'=>$session->read('visits'),
    'form'=>$request->getParsedBody(), 'files'=>$files]);
''')
    session = web.Session()
    try:
        session.call('scan')
        c = copy.deepcopy(session.snapshot['config'])
        packages = {p['version']: p['id'] for p in session.snapshot['packages']}
        c.update(http_port=28280, http_address='127.0.0.1')
        for i, p in enumerate(c['php']):
            p['port'] = 29300 + i * 8
            p['directory_entry'].update(enabled=True, port=28274+i, directory='www', address='127.0.0.1')
            if web.EXTERNAL_SITES:
                p['directory_entry']['directory'] = str(SITE_ROOT / 'www')
        c['projects'] = [dict(id='cake4', name='CakePHP 4', directory='www/cake4', document_root='webroot', php=packages['8.3.35'], enabled=True),
                         dict(id='cake5', name='CakePHP 5', directory='www/cake5', document_root='webroot', php=packages['8.5.11'], enabled=True)]
        if web.EXTERNAL_SITES:
            for project in c['projects']:
                project['directory'] = str(SITE_ROOT / project['directory'])
        session.call('apply', config=c)
        for p in c['php']:
            native = session.call('read_config', id='php:' + p['package'])
            session.call('save_config', id='php:' + p['package'], text=native['text'] + '\nextension=php_intl.dll\nextension=php_mbstring.dll\n')
        session.call('start', id='apache')
        for name, version in [('cake4','8.3.35'), ('cake5','8.5.11')]:
            ports = [28280, next(p['directory_entry']['port'] for p in c['php'] if p['package'] == packages[version])]
            for port in ports:
                status, headers, body = session.get(port, f'/{name}/users/view/42')
                assert status == 200, (port, status, body)
                assert body.startswith('{'), (port, body)
                data = json.loads(body)
                assert data['base'] == '/'+name, data
                assert data['url'] == f'/{name}/users/view/42', data
                assert f'/{name}/' in data['css'], data
                assert data['params']['id'] == '42' and data['php'] == version, data
                assert session.get(port, f'/{name}/css/app.css')[2] == 'body { color: #123; }'
                cookie = headers['Set-Cookie'].split(';')[0]
                assert json.loads(session.get(port, f'/{name}/login', headers={'Cookie':cookie})[2])['visits'] == 2
                status, headers, _ = session.get(port, f'/{name}/login?redirect=1')
                assert status == 302 and headers['Location'] == f'/{name}/login', headers
                body = '--fixture\r\nContent-Disposition: form-data; name="title"\r\n\r\nhello\r\n--fixture\r\nContent-Disposition: form-data; name="file"; filename="sample.txt"\r\nContent-Type: text/plain\r\n\r\nupload-test\r\n--fixture--\r\n'
                status, _, response = session.get(port, f'/{name}/upload', method='POST', body=body, headers={'Content-Type':'multipart/form-data; boundary=fixture'})
                data = json.loads(response)
                assert status == 200 and data['form']['title'] == 'hello' and data['files']['file'] == ['sample.txt','upload-test'], data
                web.passed(f'{name} PHP {version} on port {port}: routes, URL helpers, assets, sessions, redirects and uploads')
    finally:
        try: session.call('stop_all')
        finally:
            session.process.stdin.close(); session.process.wait(timeout=15)
        evidence = 'external-cake-paths.json' if web.EXTERNAL_SITES else 'cake-paths.json'
        web.write(web.WORKSPACE / 'testwork/evidence' / evidence, json.dumps(dict(environment=str(web.ROOT), site_root=str(SITE_ROOT), frameworks=frameworks, results=web.RESULTS),ensure_ascii=False,indent=2))
        print('ENVIRONMENT', web.ROOT)


if __name__ == '__main__': main()
