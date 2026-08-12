import json
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
                "import json, sys\nrequest=json.loads(sys.stdin.readline())\n"
                "print(json.dumps({'ok': True, 'result': {'kind': 'ack', 'session': request['session_id']}}), flush=True)\n"
            )
            session = uuid4()
            with KiteframeClient(session, executable=[sys.executable, str(harness)]) as client:
                result = client.call("session/list", {})
            self.assertEqual(result.kind, "ack")
            self.assertEqual(result.value["session"], str(session))


if __name__ == "__main__":
    unittest.main()
