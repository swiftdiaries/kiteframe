import json
import os
import sys
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory
from uuid import uuid4

sys.path.insert(0, str(Path(__file__).parents[1] / "src"))

from kiteframe import KiteframeClient


class ClientTests(unittest.TestCase):
    def test_sends_the_session_envelope_and_decodes_result(self) -> None:
        with TemporaryDirectory() as directory:
            harness = Path(directory) / "harness.py"
            harness.write_text(
                "import json, sys\n"
                "for line in sys.stdin:\n"
                " request=json.loads(line)\n"
                " kind='initialized' if request['method']=='initialize' else 'ack'\n"
                " print(json.dumps({'ok': True, 'result': {'kind': kind, 'session': request['session_id']}}), flush=True)\n"
            )
            session = uuid4()
            with KiteframeClient(session, executable=[sys.executable, str(harness)]) as client:
                result = client.call("session/list", {})
            self.assertEqual(result.kind, "ack")
            self.assertEqual(result.value["session"], str(session))

    def test_calls_the_compiled_rust_harness(self) -> None:
        database_url = os.environ.get("DATABASE_URL")
        harness = Path(__file__).parents[3] / "target" / "debug" / "kiteframe-harness"
        if not database_url or not harness.is_file():
            self.skipTest("DATABASE_URL and compiled harness are required")
        old_environment = {name: os.environ.get(name) for name in (
            "KITEFRAME_DATABASE_URL", "KITEFRAME_PRINCIPAL", "KITEFRAME_RESPONSES_BASE_URL",
            "KITEFRAME_RESPONSES_API_KEY", "KITEFRAME_RESPONSES_MODEL",
        )}
        os.environ.update({
            "KITEFRAME_DATABASE_URL": database_url,
            "KITEFRAME_PRINCIPAL": "python-smoke",
            "KITEFRAME_RESPONSES_BASE_URL": "https://api.example.test/v1",
            "KITEFRAME_RESPONSES_API_KEY": "not-a-real-key",
            "KITEFRAME_RESPONSES_MODEL": "test-model",
        })
        try:
            with KiteframeClient(uuid4(), executable=str(harness)) as client:
                result = client.call("session/list", {})
            self.assertEqual(result.kind, "sessions")
        finally:
            for name, value in old_environment.items():
                if value is None:
                    os.environ.pop(name, None)
                else:
                    os.environ[name] = value


if __name__ == "__main__":
    unittest.main()
