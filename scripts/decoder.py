from serial import Serial
from scripts.sim.sim_serial import SimSerial
from pathlib import Path
import os
import yaml
import json
from datetime import datetime
from dataclasses import dataclass, field
import scripts.db_write as dbw

DATAGRAM_SOF = b'\xaa'
DATAGRAM_EOF = b'\xbb'
PRIORITY_BIT = 0x400

id_list = []

class State:
    SOF = "SOF"
    ID = "ID"
    DLC = "DLC"
    DATA = "DATA"
    EOF = "EOF"
    VALID = "VALID"

@dataclass
class Datagram:
    pass
    idx: int = 0
    length: int = 0
    config_path: str = ""
    data: dict[str, dict[str, int]] = field(default_factory=dict)

class Decoder:
    def __init__(self, port, baudrate, timeout=1, ser=None):
        if ser is None:
            self.ser = Serial(port=port, baudrate=baudrate, timeout=timeout)
        else:
            self.ser = ser
        
        self.state = State.SOF
        self.datagram = None
        self.buffer = []
        self.decoded_data = {}
        self.isWriteToDb = False

    def enable_write_to_db(self):
        self.isWriteToDb = True
        
    def disable_write_to_db(self):
        self.isWriteToDb = False

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
            self.decode_datagram()
        return True
    
    def decode_datagram(self):
        decoded_data = Datagram()
        decoded_data.idx = self.datagram["id"]
        decoded_data.length = self.datagram["DLC"]

        with open("./boards/global_can.yaml", "r") as file:
            data_yaml = yaml.safe_load(file)

        if "messages" not in data_yaml:
            raise KeyError(f"'messages' key not found in YAML: {decoded_data.config_path}")

        # Find the matching message by CAN ID
        matched_message = None
        for message in data_yaml["messages"]:
            print(message["id"], decoded_data.idx)
            if message["id"] == decoded_data.idx:
                matched_message = message
                parent_name = message["name"]
                print(parent_name)
                break

        if matched_message is None:
            return
            # raise ValueError(f"Message ID {decoded_data.idx} not found in YAML: {decoded_data.config_path}")

        message_name = matched_message["name"]
        

        # Convert raw payload bytes to a single little-endian integer.
        # This matches DBC Intel/little-endian signals (@1+), which your YAML came from.
        payload_bytes = self.datagram["DATA"]

        # If DATA might be a list of ints instead of bytes, convert it
        if isinstance(payload_bytes, list):
            payload_bytes = bytes(payload_bytes)

        payload_int = int.from_bytes(payload_bytes, byteorder="little", signed=False)

        decoded_data.data = {}

        print("matched message")

        for signal in matched_message["signals"]:
            signal_name = signal["name"]
            start_bit = signal["start_bit"]
            bit_length = signal["length"]

            mask = (1 << bit_length) - 1
            raw_value = (payload_int >> start_bit) & mask

            decoded_data.data[signal_name] = {
                "start_bit": start_bit,
                "length": bit_length,
                "value": raw_value,
            }

            # Write each decoded signal to its own JSON file
            self._append_signal_json(
                output_dir=os.path.join("decoded_json", parent_name),
                message_name=message_name,
                signal_name=signal_name,
                value=raw_value,
                datagram_id=decoded_data.idx,
            )

        self.decoded_data[message_name] = decoded_data.data

        if self.isWriteToDb:
            dbw.write_dict(message_name, decoded_data.data, verbosity=False)

        return decoded_data

    def parse_byte(self, byte):
        print(f"{byte:#04X} State: {self.state}")
        if self.state == State.SOF or self.state == State.VALID:
            self.reset_buffer()
            if byte == 0xAA:
                self.state = State.ID
        elif self.state == State.ID:
            self.buffer.append(byte)
            if len(self.buffer) == 2:
                message_id = int.from_bytes(self.buffer, byteorder="big")
                # print(message_id)
                if not id_list.__contains__(message_id):
                    id_list.append(message_id)
                
                # print(id_list)
                self.datagram = {"id": message_id}
                # print(f"ID: {self.datagram["id"]} | DEVICE: {self.datagram["device"]}")
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
                # print(self.datagram["DATA"])
                self.state = State.EOF
        elif self.state == State.EOF:
            if byte == 0xBB:
                self.state = State.VALID
            else:
                self.state = State.SOF
        
        return self.state == State.VALID
    
    def resolve_id_to_config_path(self, id):
        directory = Path(__file__).parent / "../boards"

        for filename in os.listdir(directory):
            path = os.path.join(directory, filename)
            with open(path, 'r') as file:
                data = yaml.safe_load(file)

            for name, message in data["Messages"].items():
                if message["id"] == id:
                    return path, filename
        return "no path resolved", ""
    
    
    def _append_signal_json(self, output_dir, message_name, signal_name, value, datagram_id):
        """
        Appends one decoded signal sample to its own JSON file.
        Each file contains a JSON array of timestamped samples.
        """
        os.makedirs(output_dir, exist_ok=True)

        # Use message + signal to avoid collisions like x_axis appearing in multiple messages
        filename = f"{message_name}__{signal_name}.json"
        filepath = os.path.join(output_dir, filename)

        record = {
            "timestamp": datetime.utcnow().isoformat() + "Z",
            "id": datagram_id,
            "value": value,
        }

        if os.path.exists(filepath):
            try:
                with open(filepath, "r") as f:
                    existing = json.load(f)
                if not isinstance(existing, list):
                    existing = [existing]
            except (json.JSONDecodeError, FileNotFoundError):
                existing = []
        else:
            existing = []

        existing.append(record)

        with open(filepath, "w") as f:
            json.dump(existing, f, indent=2)