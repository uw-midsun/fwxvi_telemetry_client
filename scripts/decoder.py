from serial import Serial
import yaml
from datetime import datetime, timezone
from dataclasses import dataclass, field
import scripts.db_write as dbw
from scripts.sqlite_signal_store import SignalSampleStore

DATAGRAM_SOF = b"\xaa"
DATAGRAM_EOF = b"\xbb"
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
    def __init__(
        self,
        port,
        baudrate,
        timeout=1,
        ser=None,
        debug_mode=False,
        filter_id=None,
    ):
        if ser is None:
            self.ser = Serial(port=port, baudrate=baudrate, timeout=timeout)
        else:
            self.ser = ser

        self.state = State.SOF
        self.datagram = None
        self.buffer = []
        self.decoded_data = {}
        self.isWriteToDb = False
        self.signal_store = SignalSampleStore()
        self.debug_mode = debug_mode
        self.filter_id = filter_id

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
        if self.filter_id is not None and self.datagram["id"] != self.filter_id:
            return None

        decoded_data = Datagram()
        decoded_data.idx = self.datagram["id"]
        decoded_data.length = self.datagram["DLC"]
        self.signal_store.increment_stat("parsed_messages")

        with open("./can/global_can.yaml", "r") as file:
            data_yaml = yaml.safe_load(file)

        if "messages" not in data_yaml:
            raise KeyError(
                f"'messages' key not found in YAML: {decoded_data.config_path}"
            )

        # Find the matching message by CAN ID
        matched_message = None
        for message in data_yaml["messages"]:
            if message["id"] == decoded_data.idx:
                matched_message = message
                parent_name = message["name"]
                break

        if matched_message is None:
            self.signal_store.increment_stat("unmatched_messages")
            return

        self.signal_store.increment_stat("matched_messages")

        message_name = matched_message["name"]

        payload_bytes = self.datagram["DATA"]

        if isinstance(payload_bytes, list):
            payload_bytes = bytes(payload_bytes)

        payload_int = int.from_bytes(payload_bytes, byteorder="little", signed=False)

        decoded_data.data = {}

        sample_timestamp = datetime.now(timezone.utc).isoformat()

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

            self.signal_store.insert_signal(
                timestamp=sample_timestamp,
                can_id=decoded_data.idx,
                parent_name=parent_name,
                message_name=message_name,
                signal_name=signal_name,
                value=raw_value,
            )

        self.decoded_data[message_name] = decoded_data.data

        if self.isWriteToDb:
            dbw.write_dict(message_name, decoded_data.data, verbosity=False)

        return decoded_data

    def parse_byte(self, byte):
        if self.debug_mode:
            print(f"{byte:#04X} State: {self.state}")
        if self.state == State.SOF or self.state == State.VALID:
            self.reset_buffer()
            if byte == 0xAA:
                self.state = State.ID
        elif self.state == State.ID:
            self.buffer.append(byte)
            if len(self.buffer) == 2:
                message_id = int.from_bytes(self.buffer, byteorder="big")
                if not id_list.__contains__(message_id):
                    id_list.append(message_id)
                self.datagram = {"id": message_id}
                if self.debug_mode:
                    print(f"ID: {message_id}")
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
            if byte == 0xAA or byte == 0xBB:
                if self.debug_mode:
                    print(f"MALFORMED_MESSAGE, id: {self.datagram["id"]}")
                self.signal_store.increment_stat("malformed message")
            self.buffer.append(byte)
            if len(self.buffer) == self.datagram["DLC"]:
                self.datagram["DATA"] = bytes(self.buffer)
                self.state = State.EOF
        elif self.state == State.EOF:
            if byte == 0xBB or byte == 0:
                self.state = State.VALID
            else:
                if self.debug_mode:
                    print(f"MALFORMED_MESSAGE, id: {self.datagram["id"]}")
                self.signal_store.increment_stat("malformed message")
                self.state = State.SOF

        return self.state == State.VALID
    def close(self):
        if hasattr(self, "signal_store") and self.signal_store is not None:
            self.signal_store.close()
            self.signal_store = None
