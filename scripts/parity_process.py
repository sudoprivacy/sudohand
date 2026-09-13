"""Bounded subprocess capture for CLIs that leave browser processes running."""
import subprocess
import tempfile


def run_capture(command, *, env=None, timeout=45):
    # A descendant may inherit stdout/stderr on Windows. communicate() waits
    # for pipe EOF even after its timeout killed the original process. Files
    # let us wait on the actual command and retain its diagnostics independently.
    with tempfile.TemporaryFile() as stdout, tempfile.TemporaryFile() as stderr:
        try:
            completed = subprocess.run(command, env=env, stdout=stdout, stderr=stderr, timeout=timeout)
        except subprocess.TimeoutExpired as error:
            stdout.seek(0)
            stderr.seek(0)
            error.output = stdout.read().decode('utf-8', errors='replace')
            error.stderr = stderr.read().decode('utf-8', errors='replace')
            raise
        stdout.seek(0)
        stderr.seek(0)
        return subprocess.CompletedProcess(command, completed.returncode,
                                           stdout.read().decode('utf-8'),
                                           stderr.read().decode('utf-8'))
