"""Check predecessor selection without building or changing any release archive."""
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('builder', Path(__file__).with_name('package.py'))
builder = importlib.util.module_from_spec(spec)
spec.loader.exec_module(builder)


class ReleasePolicyTests(unittest.TestCase):
    def minimum(self, version, changelog):
        return builder.upgrade_minimum(version, {'minimum_version': '0.0.1'}, changelog)

    def test_legacy_release_keeps_its_range(self):
        self.assertEqual(self.minimum('0.0.3', '# 0.0.2\n# 0.0.1\n'), '0.0.1')

    def test_first_restricted_release_ignores_unpublished_headings(self):
        self.assertEqual(self.minimum('0.0.4', '# 0.0.4（未发布）\n# 0.0.3\n# 0.0.2\n'), '0.0.3')

    def test_version_gaps_use_latest_published_version_in_numeric_order(self):
        self.assertEqual(self.minimum('0.1.0', '# 0.0.11（未发布）\n# 0.0.9\n# 0.0.10\n'), '0.0.10')
        self.assertEqual(self.minimum('0.0.5', '# 0.0.5\n# 0.0.4\n# 0.0.3\n'), '0.0.4')

    def test_missing_predecessor_refuses_packaging(self):
        with self.assertRaises(ValueError):
            self.minimum('0.0.4', '# 未发布\n# 0.0.3（未发布）\n')

    def test_invalid_versions_refuse_packaging(self):
        for version in ['0.0.04', '0.0.4-beta', '0.4', '65536.0.0']:
            with self.subTest(version=version), self.assertRaises(ValueError):
                self.minimum(version, '# 0.0.3\n')


if __name__ == '__main__':
    unittest.main()
