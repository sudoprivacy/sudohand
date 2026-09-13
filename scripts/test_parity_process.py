import json
import os
import signal
import sys
import time
import unittest

from parity_process import run_capture, capture_process_tree, finish_process_tree


class CaptureTests(unittest.TestCase):
    def test_owned_descendants_exit_before_fixture_cleanup(self):
        import subprocess
        process = subprocess.Popen([sys.executable, '-c', "import subprocess,sys,time; child=subprocess.Popen([sys.executable,'-c','import time; time.sleep(30)']); print(child.pid,flush=True); time.sleep(30)"], stdout=subprocess.PIPE, text=True)
        try:
            descendant = int(process.stdout.readline())
            owned = capture_process_tree(process.pid)
            self.assertIn(descendant, [item.pid for item in owned])
            finish_process_tree(owned, terminate=True)
            process.wait(timeout=5)
        finally:
            if process.poll() is None:
                finish_process_tree(capture_process_tree(process.pid), terminate=True)
                process.wait(timeout=5)
            process.stdout.close()

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
