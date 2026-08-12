import os
import shutil


def harness_path() -> str:
    configured = os.environ.get("KITEFRAME_HARNESS")
    if configured:
        return configured
    discovered = shutil.which("kiteframe-harness")
    if discovered:
        return discovered
    raise RuntimeError("kiteframe-harness is not installed; set KITEFRAME_HARNESS")
