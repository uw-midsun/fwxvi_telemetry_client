import time
import threading
from queue import Queue, Empty

class SimSerial:

    def __init__(self, baudrate=115200):
        self.rx = Queue()
        self.is_open = False
        self.baudrate = baudrate

    def open(self):
        self.is_open = True

    def feed(self, data: bytes, paced: bool = False):
        for b in data:
            self.rx.put(bytes([b]))


    def read(self, size=1):
        if self.is_open != True:
            raise Exception("Serial port not open yet...")
        
        chunks = []

        for i in range (0, size):
            try:
                chunks.append(self.rx.get())
            except Empty:
                break
        
        out = b"".join(chunks)

        if size > 0:
            return out[:size]
        
        return out