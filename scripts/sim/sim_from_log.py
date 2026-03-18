from collections import deque
from datetime import datetime
from pathlib import Path
import time


class SimFromLog:
    def __init__(self, log_path, baudrate=115200):
        self.log_path = Path(log_path)
        self.baudrate = baudrate
        self.bytes_per_second = max(1.0, float(baudrate / 8))
        self.byte_interval = 1.0 / self.bytes_per_second

        self.events = self._parse_log(self.log_path)
        self.event_idx = 0

        self.rx = deque()
        self.is_open = False
        self.start_time = 0.0
        self.next_byte_time = 0.0

    def _parse_log(self, log_path):
        events = []
        first_timestamp = None

        with log_path.open("r", encoding="utf-8") as handle:
            for line in handle:
                line = line.strip()
                if not line or ",RECV," not in line:
                    continue

                parts = line.split(",", 3)
                if len(parts) != 4:
                    continue

                timestamp_text, _, message_type, payload_hex = parts
                if message_type.strip() != "RECV":
                    continue

                payload_hex = payload_hex.strip()
                if not payload_hex:
                    continue

                try:
                    payload = bytes.fromhex(payload_hex)
                    timestamp = datetime.strptime(timestamp_text.strip(), "%m-%d-%Y %H:%M:%S.%f")
                except ValueError:
                    continue

                if first_timestamp is None:
                    first_timestamp = timestamp

                relative_seconds = (timestamp - first_timestamp).total_seconds()
                events.append((relative_seconds, payload))

        return events

    def open(self):
        self.is_open = True
        self.start_time = time.monotonic()
        self.next_byte_time = self.start_time

    def _fill_ready_events(self):
        now = time.monotonic()
        elapsed = now - self.start_time

        while self.event_idx < len(self.events):
            event_time, payload = self.events[self.event_idx]
            if elapsed < event_time:
                break
            self.rx.extend(payload)
            self.event_idx += 1

    def read(self, size=1):
        if not self.is_open:
            raise Exception("Serial port not open yet...")

        if size <= 0:
            return b""

        output = bytearray()

        while len(output) < size:
            self._fill_ready_events()
            now = time.monotonic()

            if self.rx and now >= self.next_byte_time:
                output.append(self.rx.popleft())
                self.next_byte_time = now + self.byte_interval
                continue

            if not self.rx and self.event_idx >= len(self.events):
                break

            sleep_for = 0.002
            if self.rx:
                sleep_for = min(sleep_for, max(0.0, self.next_byte_time - now))
            else:
                next_event_time = self.start_time + self.events[self.event_idx][0]
                sleep_for = min(sleep_for, max(0.0, next_event_time - now))

            time.sleep(max(0.0005, sleep_for))

        return bytes(output)

    def close(self):
        self.is_open = False
