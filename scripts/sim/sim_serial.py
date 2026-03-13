import time
from queue import Queue, Empty

class SimSerial:

    def __init__(self, baudrate=115200):
        self.rx = Queue()
        self.is_open = False
        self.baudrate = baudrate
        self.bytes_per_second = max(1.0, (float)(baudrate/8))

    def open(self):
        self.is_open = True

    def feed(self, data: bytes, paced: bool = False):
        delay = 0

        if paced:
            delay = 1.0 / self.bytes_per_second

        for b in data:
            self.rx.put(bytes([b]))
            time.sleep(delay)


    def read(self, size=1):
        if self.is_open != True:
            raise Exception("Serial port not open yet...")
        
        chunks = []

        for i in range (0, size):
            try:
                chunks.append(self.rx.get(timeout=0.02))
            except Empty:
                break
        
        out = b"".join(chunks)

        if size > 0:
            return out[:size]
        
        return out