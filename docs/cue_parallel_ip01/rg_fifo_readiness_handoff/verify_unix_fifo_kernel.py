#!/usr/bin/env python3
"""Linux/Unix FIFO OS-semantics smoke test; NOT a compiled TonePoet test.

Probes kernel behavior assumed by the Rust implementation (not the Rust
implementation itself). Run independently on the destination host.
"""
import errno
import os
import select
import tempfile
import threading
import time
import unittest
from pathlib import Path

EVENTS = select.POLLIN | select.POLLHUP | select.POLLERR


class FifoKernelTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="rg-fifo-kernel-")
        self.addCleanup(self.temp.cleanup)
        self.path = Path(self.temp.name) / "mirror.fifo"
        os.mkfifo(self.path, 0o600)
        self.reader = os.open(self.path, os.O_RDONLY | os.O_NONBLOCK)
        self.addCleanup(os.close, self.reader)
        self.poller = select.poll()
        self.poller.register(self.reader, EVENTS)

    def test_disconnected_pipe_can_be_hung_up_after_writer_closes(self):
        # A never-connected FIFO can poll as *not* readable on Linux even
        # while read(2) returns EOF. Avoid asserting any fixed initial revents.
        self.assertEqual(os.read(self.reader, 1), b"")
        writer = os.open(self.path, os.O_WRONLY | os.O_NONBLOCK)
        os.close(writer)
        events = self.poller.poll(50)
        self.assertTrue(events, "closed writer produces persistent hangup")
        self.assertTrue(events[0][1] & select.POLLHUP)
        self.assertEqual(os.read(self.reader, 1), b"")
        # Correct code must NOT repoll this state without a backoff.

    def test_attached_idle_writer_times_out_then_wakes_on_write(self):
        writer = os.open(self.path, os.O_WRONLY | os.O_NONBLOCK)
        try:
            self.assertEqual(self.poller.poll(25), [])
            with self.assertRaises(BlockingIOError) as ctx:
                os.read(self.reader, 1)
            self.assertEqual(ctx.exception.errno, errno.EAGAIN)
            os.write(writer, b"ABC")
            self.assertTrue(self.poller.poll(50))
            self.assertEqual(os.read(self.reader, 2), b"AB")
        finally:
            os.close(writer)
        # The final buffered byte must survive writer hangup.
        self.assertTrue(self.poller.poll(50))
        self.assertEqual(os.read(self.reader, 10), b"C")
        self.assertEqual(os.read(self.reader, 1), b"")

    def test_irregular_chunks_and_cancellable_idle_wait(self):
        writer = os.open(self.path, os.O_WRONLY | os.O_NONBLOCK)
        cancel = threading.Event()
        payload = bytes(i % 251 for i in range(128 * 1024))
        def send():
            try:
                for start in range(0, len(payload), 2001):
                    chunk = memoryview(payload)[start:start + 2001]
                    while chunk:
                        try:
                            sent = os.write(writer, chunk)
                            chunk = chunk[sent:]
                        except BlockingIOError:
                            time.sleep(0.0005)  # test fixture only
            finally:
                os.close(writer)
                cancel.set()
        worker = threading.Thread(target=send)
        worker.start()
        observed = bytearray()
        waits = 0
        while len(observed) < len(payload):
            try:
                data = os.read(self.reader, min(65536, len(payload) - len(observed)))
                if not data:
                    self.fail("unexpected EOF during payload")
                observed.extend(data)
            except BlockingIOError:
                waits += 1
                if self.poller.poll(50):
                    continue
                self.assertFalse(cancel.is_set(), "writer ended without completing payload")
        worker.join(timeout=4)
        self.assertFalse(worker.is_alive())
        self.assertEqual(observed, payload)
        self.assertEqual(os.read(self.reader, 1), b"")
        self.assertGreaterEqual(waits, 0)  # scheduling-dependent: zero is valid


if __name__ == "__main__":
    unittest.main(verbosity=2)
