"""Check explicit compatibility ranges without changing release archives."""
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('builder', Path(__file__).with_name('package.py'))
builder = importlib.util.module_from_spec(spec)
spec.loader.exec_module(builder)


class ReleasePolicyTests(unittest.TestCase):
    def minimum(self, version, changelog, minimum='0.0.1', excluded=()):
        return builder.upgrade_minimum(version, {'minimum_version': minimum, 'excluded_versions': excluded}, changelog)

    def test_legacy_release_keeps_its_range(self):
        self.assertEqual(self.minimum('0.0.3', '# 0.0.2\n# 0.0.1\n'), '0.0.1')

    def test_new_releases_do_not_silently_raise_minimum(self):
        self.assertEqual(self.minimum('0.0.5', '# 0.0.5（未发布）\n# 0.0.4\n# 0.0.1\n'), '0.0.1')

    def test_version_gaps_use_explicit_minimum(self):
        self.assertEqual(self.minimum('0.1.0', '# 0.0.4\n# 0.0.9\n', '0.0.4', ['0.0.9']), '0.0.4')
        self.assertEqual(self.minimum('0.0.1', '# 未发布\n'), '0.0.1')

    def test_missing_published_minimum_refuses_packaging(self):
        with self.assertRaises(ValueError):
            self.minimum('0.0.4', '# 未发布\n# 0.0.3（未发布）\n')

    def test_invalid_versions_refuse_packaging(self):
        for version in ['0.0.04', '0.0.4-beta', '0.4', '65536.0.0']:
            with self.subTest(version=version), self.assertRaises(ValueError):
                self.minimum(version, '# 0.0.3\n')

    def test_invalid_ranges(self):
        for minimum, excluded in [('0.0.6', []), ('0.0.1', ['0.0.0']), ('0.0.1', ['0.0.5']), ('0.0.1', ['bad'])]:
            with self.subTest(minimum=minimum, excluded=excluded), self.assertRaises(ValueError):
                self.minimum('0.0.5', '# 0.0.1\n# 0.0.6\n', minimum, excluded)


if __name__ == '__main__':
    unittest.main()
