"""Verify directory listings using isolated Apache and PHP packages."""
import copy
import importlib.util
import json
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    'php_version', Path(__file__).with_name('acceptance-php-version.py'))
fixtures = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixtures)
web = fixtures.web


def main():
    web.checked(web.ROOT).mkdir(parents=True)
    fixtures.copy_package('apache', 'apache-2.4.69')
    fixtures.copy_package('php', 'php-8.3.35')
    session = web.Session()
    results = []
    try:
        session.call('scan')
        packages = session.snapshot['packages']
        assert all(p['error'] is None for p in packages), packages
        config = copy.deepcopy(session.snapshot['config'])
        http_port, php_port, directory_port = fixtures.free_ports(3)
        config.update(http_port=http_port, http_address='127.0.0.1')
        php = config['php'][0]
        php.update(port=php_port, workers=1, usage='normal', directory_entry=dict(
            enabled=True, port=directory_port, directory='www/dev library/public',
            address='127.0.0.1'))
        config['projects'] = [dict(id='library', directory='www/dev library',
            document_root='public', php=php['package'], enabled=True, listed=True,
            lan_access=False, virtual_host=True, domain='library.localhost')]
        site = web.ROOT / 'www/dev library/public'
        web.write(site / 'sample.txt', 'directory-browsing-fixture')
        web.write(site / 'nested/child.txt', 'nested-fixture')
        web.write(site / 'run.php', '<?php echo "executed-" . PHP_VERSION;')
        web.write(site / 'php-home/index.php', '<?php echo "php-home";')
        web.write(site / 'html-home/index.html', 'html-home')
        web.write(site / 'blocked/.htaccess', 'Options -Indexes\n')
        web.write(site / 'blocked/hidden.txt', 'not-listed')
        web.write(site / 'rewritten/.htaccess',
                  'RewriteEngine On\nRewriteRule ^$ home.php [L]\n')
        web.write(site / 'rewritten/home.php', '<?php echo "rewrite-home";')
        web.write(site.parent / 'private.txt', 'outside-document-root')
        session.call('apply', config=config)

        # Model a persisted managed configuration from a build with indexes disabled.
        conf = web.checked(web.ROOT / 'config/services/apache/httpd.conf')
        baseline = web.checked(conf.with_suffix('.managed.json'))
        metadata = json.loads(baseline.read_text(encoding='utf-8'))
        original = conf.read_text(encoding='utf-8')
        old_head = ''.join(line for line in metadata['head'].splitlines(keepends=True)
                           if not line.startswith('LoadModule autoindex_module '))
        old_tail = metadata['tail'].replace('Options +Indexes', 'Options -Indexes')
        body = original[len(metadata['head']):len(original) - len(metadata['tail'])]
        body += '\n# directory-browsing custom setting\nIndexOptions FancyIndexing\n'
        web.write(conf, old_head + body + old_tail)
        metadata.update(head=old_head, tail=old_tail)
        web.write(baseline, json.dumps(metadata))
        session.call('start', id='apache')
        active = conf.read_text(encoding='utf-8')
        assert 'LoadModule autoindex_module ' in active
        assert '# directory-browsing custom setting\nIndexOptions FancyIndexing' in active
        results.append('Starting with existing managed configuration enables autoindex and preserves custom settings')

        entries = [(directory_port, '/', 'localhost'),
                   (http_port, '/library/', 'localhost'),
                   (http_port, '/', 'library.localhost')]
        for port, prefix, host in entries:
            def get(path=''):
                return session.get(port, prefix + path, host=host)

            status, _, listing = get()
            assert status == 200 and 'sample.txt' in listing and 'nested/' in listing, listing
            assert 'private.txt' not in listing and '<?php' not in listing
            assert 'child.txt' in get('nested/')[2]
            assert get('sample.txt')[2] == 'directory-browsing-fixture'
            assert get('run.php')[2] == 'executed-8.3.35'
            assert get('php-home/')[2] == 'php-home'
            assert get('html-home/')[2] == 'html-home'
            assert get('blocked/')[0] == 403
            assert get('rewritten/')[2] == 'rewrite-home'
            assert get('private.txt')[0] == 404
        results.append('All three entries list directories, execute PHP, prefer home pages and respect htaccess overrides')
        assert 'href="/library/"' in session.get(http_port)[2]
        assert session.get(http_port, '/missing/')[0] == 404
        results.append('Project navigation and unknown-path 404 remain available')
        session.call('reload', id='apache')
        session.call('stop_all')
        session.call('start', id='apache')
        for port, prefix, host in entries:
            status, _, listing = session.get(port, prefix, host=host)
            assert status == 200 and 'sample.txt' in listing
        results.append('Directory listings survive Apache reload and full service restart')
    finally:
        try:
            session.call('stop_all')
            assert not any(row['running'] for row in session.snapshot['rows'])
        finally:
            session.process.stdin.close()
            session.process.wait(timeout=15)
    evidence = dict(environment=str(web.ROOT), executable=str(web.EXE),
                    packages=packages, results=results, stopped=True)
    web.write(web.WORKSPACE / 'testwork/evidence/directory-browsing.json',
              json.dumps(evidence, ensure_ascii=False, indent=2))
    print(json.dumps(dict(results=results, stopped=True), ensure_ascii=False, indent=2))


if __name__ == '__main__':
    main()
