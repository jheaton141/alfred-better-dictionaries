# -*- coding: utf-8 -*-

import os
import sys
import time
from subprocess import Popen, PIPE, DEVNULL

from multiprocess import Process, Queue

import alfred

WORKFLOW_DIR = alfred.get_workflow_dir()


def _notify(title, message):
    """Report progress with a native macOS notification (osascript, works on
    Apple Silicon without Rosetta) plus a status file the user can `cat`."""
    msg = message.replace("\\", "").replace('"', "'")[:120]
    ttl = title.replace("\\", "").replace('"', "'")
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
    """Notification every 10%; no external helper app needed."""

    def __init__(self, title="Progress", message="", percent=0.01):
        self.title = title
        self.start_time = time.time()
        self.last_decile = -1
        _notify(title, "started")

    def update(self, percent, message):
        decile = int(percent) // 10
        if decile <= self.last_decile:
            return
        self.last_decile = decile
        elapsed = time.time() - self.start_time
        remaining = int(elapsed * 100 / max(percent, 0.01) - elapsed)
        _notify(self.title,
                f"{int(percent)}%  [ETA {remaining // 60:02d}:{remaining % 60:02d}] {message}")

    def finish(self):
        _notify(self.title, "done")


class IndefiniteProgressBar:
    def __init__(self, title="Progress", message=""):
        self.title = title
        self.start_time = time.time()
        self.last_notified = 0.0
        _notify(title, message or "working…")

    def update(self, message):
        now = time.time()
        if now - self.last_notified < 30:
            return
        self.last_notified = now
        e = int(now - self.start_time)
        _notify(self.title, f"[Elapsed {e // 60:02d}:{e % 60:02d}] {message}")

    def finish(self):
        _notify(self.title, "done")


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
