from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path


def _load_helper() -> object:
    helper_path = Path(__file__).with_name("isolated_database.py")
    spec = importlib.util.spec_from_file_location("isolated_database", helper_path)
    assert spec is not None
    assert spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class IsolatedDatabaseUrlTests(unittest.TestCase):
    def test_replaces_only_the_database_path_and_preserves_connection_options(self) -> None:
        helper = _load_helper()

        url = helper.isolated_database_url(
            "postgres://postgres:postgres@127.0.0.1:5432/kiteframe_ci?sslmode=disable",
            "kiteframe_ci_python_live_123_1",
        )

        self.assertEqual(
            url,
            "postgres://postgres:postgres@127.0.0.1:5432/"
            "kiteframe_ci_python_live_123_1?sslmode=disable",
        )


if __name__ == "__main__":
    unittest.main()
