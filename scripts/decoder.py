from serial import Serial
from scripts.sim_serial import SimSerial
from pathlib import Path
import os
import yaml
from dataclasses import dataclass, field

DATAGRAM_SOF = b'\xaa'
DATAGRAM_EOF = b'\xbb'

class State:
    SOF = "SOF"
    ID = "ID"
    DLC = "DLC"
    DATA = "DATA"
    EOF = "EOM"
    VALID = "VALID"

@dataclass
class Datagram:
    pass
    idx: int = 0
    length: int = 0
    config_path: str = ""
    data: dict[str, dict[str, int]] = field(default_factory=dict)

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
            print(self.decode_datagram())
        return True
    
    def decode_datagram(self):
        data = self.datagram
        decoded_data = Datagram()
        decoded_data.idx = self.datagram["id"]
        decoded_data.length = self.datagram["DLC"]
        decoded_data.config_path = self.resolve_id_to_config_path(decoded_data.idx)

        with open(decoded_data.config_path, 'r') as file:
            data = yaml.safe_load(file)

        offset_b = 0

        for name, message in data["Messages"].items():
            if message["id"] == decoded_data.idx:
                for config_name, config_data in message["signals"].items():
                    m_data = {}

                    if config_data["length"] % 8 != 0:
                        raise Exception("Invalid length")
                    
                    m_data["len"] = config_data["length"]
                    current_length_b = int(config_data["length"] / 8)
                    m_data["value"] = int.from_bytes(self.datagram["DATA"][offset_b:offset_b+current_length_b])
                    offset_b += current_length_b
                    decoded_data.data[config_name] = m_data

        return decoded_data.data

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
                self.datagram["DATA"] = bytes(self.buffer)
                self.state = State.EOF
        elif self.state == State.EOF:
            if byte == 0xBB:
                self.state = State.VALID
            else:
                self.state = State.SOF
        
        return self.state == State.VALID
    
    def resolve_id_to_config_path(self, id):
        directory = Path(__file__).parent / f"../boards"

        for filename in os.listdir(directory):
            path = os.path.join(directory, filename)
            with open(path, 'r') as file:
                data = yaml.safe_load(file)

            for name, message in data["Messages"].items():
                if message["id"] == id:
                    return path
        return "no path resolved"