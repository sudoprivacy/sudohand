import json
import os
import signal
import sys
import time
import unittest

from parity_process import run_capture


class CaptureTests(unittest.TestCase):
    def test_descendant_does_not_hold_capture_open(self):
        program = "import subprocess,sys,json; p=subprocess.Popen([sys.executable,'-c','import time; time.sleep(30)']); print(json.dumps({'pid':p.pid}),flush=True)"
        started = time.monotonic()
        result = run_capture([sys.executable, '-c', program], timeout=5)
        child = json.loads(result.stdout)['pid']
        try:
            self.assertEqual(result.returncode, 0)
            self.assertLess(time.monotonic() - started, 5)
        finally:
            os.kill(child, signal.SIGTERM)

    def test_timeout_retains_output_and_finishes(self):
        import subprocess
        program = "import subprocess,sys,json,time; p=subprocess.Popen([sys.executable,'-c','import time; time.sleep(30)']); print(json.dumps({'pid':p.pid}),flush=True); time.sleep(30)"
        started = time.monotonic()
        with self.assertRaises(subprocess.TimeoutExpired) as caught:
            run_capture([sys.executable, '-c', program], timeout=2)
        child = json.loads(caught.exception.output)['pid']
        try:
            self.assertLess(time.monotonic() - started, 5)
        finally:
            os.kill(child, signal.SIGTERM)


if __name__ == '__main__':
    unittest.main()
