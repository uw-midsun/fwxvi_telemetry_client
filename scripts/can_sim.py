# DATAGRAM STRUCTURE:
# Start of frame (0xAA)
# ID (2 bytes)
# DLC - Data Length Code (number of bytes in the payload)
# Data - max size of 8
# End of frame (0xBB)

import yaml
from pathlib import Path

DATAGRAM_SOF = 0xAA
DATAGRAM_EOF = 0xBB

outputnums = [19, 21, 4, 55, 2, 7, 43, 88, 67, 33, 1]

class CanMessageSimulator:
    def __init__(self):
        pass

    def gen_datagram(self, yaml_name: str):
        yaml_file = Path(__file__).parent / f"../boards/{yaml_name}.yaml"
        with open(yaml_file, 'r') as file:
            data = yaml.safe_load(file)

        chunks = []

        for name, message in data["Messages"].items():
            chunks.append(DATAGRAM_SOF.to_bytes(2, byteorder="big"))
            chunks.append(message['id'].to_bytes(2, byteorder="big"))

            total_length = 0
            for signal_name, signal_data in message["signals"].items():
                total_length += signal_data["length"]

            if total_length % 8 == 0:
                total_length = (int) (total_length / 8)
            else:
                raise Exception("Invalid length")
                

            chunks.append(total_length.to_bytes(1, byteorder="big"))

            index = 0

            for signal_name, signal_data in message["signals"].items():
                if signal_data["length"] % 8 != 0:
                    raise Exception("Invalid length")
                
                chunks.append(outputnums[index].to_bytes(int(signal_data["length"] / 8), byteorder="big"))
                index += 1
            
            chunks.append(DATAGRAM_EOF.to_bytes(1, byteorder="big"))

        out = b"".join(chunks)
        return out
