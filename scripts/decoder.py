from serial import Serial
from scripts.sim_serial import SimSerial

DATAGRAM_SOF = b'\xaa'
DATAGRAM_EOF = b'\xbb'

class State:
    SOF = "SOF"
    ID = "ID"
    DLC = "DLC"
    DATA = "DATA"
    EOF = "EOM"
    VALID = "VALID"

class Decoder:
    def __init__(self, port="/dev/ttyUSB0", baudrate=115200, timeout=1, ser=None):
        if ser is None:
            self.ser = Serial(port=port, baudrate=baudrate, timeout=timeout)
        else:
            self.ser = ser
        
        self.state = State.SOF
        self.datagram = None
        self.buffer = []

    def reset_buffer(self):
        self.buffer = []
        self.datagram = None
        self.state = State.SOF

    def read(self):
        try:
            byte = self.ser.read(1)[0]
        except IndexError:
            return False
        if self.parse_byte(byte):
            print(self.datagram)
        return True

    def parse_byte(self, byte):
        if self.state == State.SOF or self.state == State.VALID:
            self.reset_buffer()
            if byte == 0xAA:
                self.state = State.ID
        elif self.state == State.ID:
            self.buffer.append(byte)
            if len(self.buffer) == 2:
                message_id = int.from_bytes(self.buffer, byteorder="big")
                self.datagram = {"id": message_id}
                self.buffer = []
                self.state = State.DLC
        elif self.state == State.DLC:
            self.datagram["DLC"] = byte
            if byte <= 9:
                self.datagram["DATA"] = []
                self.state = State.DATA
            else:
                self.state = State.SOF
        elif self.state == State.DATA:
            self.buffer.append(byte)
            if len(self.buffer) == self.datagram["DLC"]:
                self.datagram["DATA"] = self.buffer
                self.state = State.EOF
        elif self.state == State.EOF:
            if byte == 0xBB:
                self.state = State.VALID
            else:
                self.state = State.SOF
        
        return self.state == State.VALID