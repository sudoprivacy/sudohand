"""Bounded subprocess capture for CLIs that leave browser processes running."""
import subprocess
import tempfile


def capture_process_tree(pid):
    """Capture only a fixture's currently owned processes, with PID reuse checks."""
    import psutil
    try:
        root = psutil.Process(pid)
        return [root, *root.children(recursive=True)]
    except psutil.NoSuchProcess:
        return []


def finish_process_tree(processes, *, terminate=False):
    """Wait for fixture descendants before removing Windows-locked profiles."""
    import psutil

    def signal(process, method):
        try:
            if process.is_running():  # psutil checks the cached creation time.
                getattr(process, method)()
        except psutil.NoSuchProcess:
            pass

    if terminate:
        for process in processes:
            signal(process, 'terminate')
    _, alive = psutil.wait_procs(processes, timeout=5)
    for process in alive:
        signal(process, 'kill')
    _, alive = psutil.wait_procs(alive, timeout=5)
    # A reparented Unix zombie has exited and released its file handles.
    remaining = []
    for process in alive:
        try:
            if process.is_running() and process.status() != psutil.STATUS_ZOMBIE:
                remaining.append(process.pid)
        except psutil.NoSuchProcess:
            pass
    assert not remaining, f'fixture processes did not exit: {remaining}'


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
