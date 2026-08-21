from __future__ import annotations

import contextlib
import io
import importlib.util
import os
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch


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

    def test_preserves_encoded_credentials_ipv6_and_query_options_in_smoke_url(
        self,
    ) -> None:
        helper = _load_helper()

        url = helper.isolated_database_url(
            "postgresql://user%40name:password%2Fvalue@[::1]:5433/kiteframe_ci"
            "?sslmode=require&application_name=ci%20smoke",
            "kiteframe_ci_python_live_123_1",
        )

        self.assertEqual(
            url,
            "postgresql://user%40name:password%2Fvalue@[::1]:5433/"
            "kiteframe_ci_python_live_123_1?sslmode=require&application_name=ci%20smoke",
        )

    def test_create_uses_libpq_environment_without_credentials_in_argv(self) -> None:
        helper = _load_helper()
        credential = "password/sentinel"
        database_url = (
            "postgresql://user%40name:password%2Fsentinel@[::1]:5433/kiteframe_ci"
            "?sslmode=require"
        )
        calls: list[tuple[list[str], dict[str, str]]] = []

        def fake_run(arguments: list[str], **kwargs: object) -> None:
            calls.append((arguments, kwargs["env"]))

        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "github-output"
            stdout = io.StringIO()
            with (
                patch.object(helper.subprocess, "run", side_effect=fake_run),
                contextlib.redirect_stdout(stdout),
            ):
                helper.create_database(database_url, "kiteframe_ci_python_live_123_1", output)

            self.assertEqual(
                calls,
                [
                    (
                        [
                            "createdb",
                            "--maintenance-db",
                            "kiteframe_ci",
                            "kiteframe_ci_python_live_123_1",
                        ],
                        unittest.mock.ANY,
                    )
                ],
            )
            arguments, environment = calls[0]
            self.assertNotIn(credential, " ".join(arguments))
            self.assertEqual(environment["PGHOST"], "::1")
            self.assertEqual(environment["PGPORT"], "5433")
            self.assertEqual(environment["PGUSER"], "user@name")
            self.assertEqual(environment["PGPASSWORD"], credential)
            self.assertEqual(environment["PGSSLMODE"], "require")
            expected_url = (
                "postgresql://user%40name:password%2Fsentinel@[::1]:5433/"
                "kiteframe_ci_python_live_123_1?sslmode=require"
            )
            self.assertEqual(stdout.getvalue(), f"::add-mask::{expected_url}\n")
            self.assertEqual(output.read_text(encoding="utf-8"), f"database_url={expected_url}\n")

    def test_failing_command_does_not_expose_credential(self) -> None:
        helper = _load_helper()
        credential = "super-secret-password"
        database_url = f"postgres://postgres:{credential}@127.0.0.1:5432/kiteframe_ci"

        with tempfile.TemporaryDirectory() as directory:
            executable = Path(directory) / "createdb"
            executable.write_text(
                "#!/bin/sh\nprintf '%s' \"$KITEFRAME_SENTINEL\"\nprintf '%s' \"$KITEFRAME_SENTINEL\" >&2\nexit 7\n",
                encoding="utf-8",
            )
            executable.chmod(0o755)
            output = Path(directory) / "github-output"
            stdout = io.StringIO()
            stderr = io.StringIO()
            with (
                patch.dict(
                    os.environ,
                    {
                        "PATH": f"{directory}{os.pathsep}{os.environ['PATH']}",
                        "KITEFRAME_SENTINEL": credential,
                    },
                    clear=False,
                ),
                contextlib.redirect_stdout(stdout),
                contextlib.redirect_stderr(stderr),
                self.assertRaises(helper.DatabaseCommandError) as raised,
            ):
                helper.create_database(database_url, "kiteframe_ci_python_live_123_1", output)

        self.assertNotIn(credential, stdout.getvalue())
        self.assertNotIn(credential, stderr.getvalue())
        self.assertNotIn(credential, str(raised.exception))


if __name__ == "__main__":
    unittest.main()
