# -*- coding: utf-8 -*-

import os
import sys
import time
from subprocess import Popen, PIPE, DEVNULL

from multiprocess import Process, Queue

import alfred

WORKFLOW_DIR = alfred.get_workflow_dir()
CD_PATH = f"{WORKFLOW_DIR}/cocoaDialog.app/Contents/MacOS/cocoaDialog"


def _launch_cocoadialog(cmd):
    """Start the progress window. The bundled cocoaDialog is an Intel-only
    binary, so on an Apple Silicon Mac without Rosetta it cannot launch.
    Return None instead of crashing the whole import in that case."""
    try:
        proc = Popen(cmd, stdin=PIPE, stdout=DEVNULL, encoding="utf-8")
    except OSError as e:
        print(f"progress window unavailable ({e}); using notifications",
              file=sys.stderr)
        return None
    time.sleep(0.2)
    if proc.poll() is not None:  # died immediately
        return None
    return proc


def _notify(title, message):
    """Fallback progress reporting: a macOS notification plus a status file."""
    msg = message.replace('"', "'")[:120]
    ttl = title.replace('"', "'")
    try:
        Popen(
            ["osascript", "-e",
             f'display notification "{msg}" with title "BetterDict" '
             f'subtitle "{ttl}"'],
            stdout=DEVNULL, stderr=DEVNULL,
        )
    except OSError:
        pass
    data_dir = os.environ.get("alfred_workflow_data")
    if data_dir and os.path.isdir(data_dir):
        with open(f"{data_dir}/progress.txt", "w") as f:
            f.write(f"{time.ctime()}  {ttl}: {message}\n")


class ProgressBar:

    def __init__(self, title="Progress", message="", percent=0.01):
        cmd = [
            CD_PATH,
            "progressbar",
            "--title",
            title,
            "--text",
            message,
            "--percent",
            str(percent),
        ]
        self.title = title
        self.proc = _launch_cocoadialog(cmd)
        self.message = message
        self.start_time = time.time()
        self.last_notified_decile = -1
        if self.proc is None:
            _notify(title, "started")

    def update(self, percent, message):
        elapsed_seconds = time.time() - self.start_time
        estimated_total_seconds = elapsed_seconds * 100 / percent
        estimated_remaining_seconds = estimated_total_seconds - elapsed_seconds
        rs = int(estimated_remaining_seconds) % 60
        rm = int(estimated_remaining_seconds) // 60
        eta_n_message = f"[ETA {rm:02d}:{rs:02d}] {message}"
        if self.proc is None:
            decile = int(percent) // 10
            if decile > self.last_notified_decile:
                self.last_notified_decile = decile
                _notify(self.title, f"{int(percent)}%  {eta_n_message}")
            return
        try:
            self.proc.stdin.write(f"{int(percent)} {eta_n_message}\n")
            self.proc.stdin.flush()
        except (BrokenPipeError, ValueError):
            self.proc = None  # window was closed; carry on silently

    def finish(self):
        if self.proc is not None:
            self.proc.kill()


class IndefiniteProgressBar:

    def __init__(self, title="Progress", message=""):
        cmd = [
            CD_PATH,
            "progressbar",
            "--title",
            title,
            "--text",
            message,
            "--indeterminate",
        ]
        self.proc = _launch_cocoadialog(cmd)
        self.message = message
        self.start_time = time.time()

    def update(self, message):
        if self.proc is None:
            return
        elapsed_seconds = time.time() - self.start_time
        es = int(elapsed_seconds) % 60
        em = int(elapsed_seconds) // 60
        time_n_message = f"[Elapsed {em:02d}:{es:02d}] {message}"
        try:
            self.proc.stdin.write(f"0 {time_n_message}\n")
            self.proc.stdin.flush()
        except (BrokenPipeError, ValueError):
            self.proc = None

    def finish(self):
        if self.proc is not None:
            self.proc.kill()


class NoOpAcc:
    def add(self, items):
        pass

    def finish(self):
        pass


def run_parallely_with_progress_bar(
    items, func, msgfunc, accumulator=NoOpAcc(), title="", wait=False
):
    PROC_COUNT = 5

    total = len(items)

    task_queue = Queue()
    done_queue = Queue()

    def pb_updater(inq, results_q):
        pb = ProgressBar(title)
        for i in range(total):
            msg, result = results_q.get()
            accumulator.add(result)
            pb.update(percent=((i + 1) * 100) / total, message=msg)
        pb.finish()
        accumulator.finish()

        # tell the workers to stop
        for i in range(PROC_COUNT):
            inq.put("STOP")

    def worker(inq, outq):
        for item in iter(inq.get, "STOP"):
            result = func(item)
            outq.put((msgfunc(item), result))

    for i in range(PROC_COUNT):
        Process(target=worker, args=(task_queue, done_queue)).start()

    updater = Process(target=pb_updater, args=(task_queue, done_queue))
    updater.start()
    for item in items:
        task_queue.put(item)

    if wait:
        updater.join()


def _run_parallely_with_progress_bar(items, func, msgfunc, title):
    pb = ProgressBar(title)
    total = len(items)

    for i, item in enumerate(items):
        pb.update(percent=((i + 1) * 100) / total, message=msgfunc(item))
        func(item)

    pb.finish()
