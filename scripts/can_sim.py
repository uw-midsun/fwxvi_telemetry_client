# DATAGRAM STRUCTURE:
# Start of frame (0xAA)
# ID (2 bytes)
# DLC - Data Length Code (number of bytes in the payload)
# Data - max size of 8
# End of frame (0xBB)

import yaml
from pathlib import Path
from random import randint


DATAGRAM_SOF = 0xAA
DATAGRAM_EOF = 0xBB

outputnums = [19, 21, 4, 55, 2, 7, 43, 88, 67, 33, 1]

class CanMessageSimulator:
    def __init__(self):
        self.board_idx = 0
        self.fake_data = {}
        self.num_packets = 0
        self.has_read_config = False
        pass
    
    def read_config(self, config_files:list):
        
        
        for file_name in config_files:
            yaml_file = Path(__file__).parent / f"../boards/{file_name}.yaml"
            with open(yaml_file, 'r') as file:
                data = yaml.safe_load(file)
            
            for name, message in data["Messages"].items():
                field = {}
                field["id"] = message["id"]
                datagram = {}
                for signal_name, signal_data in message["signals"].items():
                    datagram[signal_name] = {"value": 0, "length": signal_data["length"]}
                field["signals"] = datagram
                self.fake_data[name] = field
            
        self.num_packets = len(self.fake_data)
        self.has_read_config = True

    def gen_datagram(self):
        index = 0
        for board, message in self.fake_data.items():
            if index != self.board_idx:
                index += 1
                continue
            
            self.board_idx += 1
            self.board_idx %= self.num_packets
            
            chunks = []
            chunks.append(DATAGRAM_SOF.to_bytes(2, byteorder="big"))
            chunks.append(message["id"].to_bytes(2, byteorder="big"))
            
            total_length = 0
            
            for signal_name, signal_data in message["signals"].items():
                total_length += signal_data["length"]

            if total_length % 8 == 0:
                total_length = (int) (total_length / 8)
            else:
                raise Exception("Invalid length")
            
            chunks.append(total_length.to_bytes(1, byteorder="big"))
            
            for signal_name, signal_data in message["signals"].items():
                if signal_data["length"] % 8 != 0:
                    raise Exception("Invalid length")
                
                try:
                    chunks.append(signal_data["value"].to_bytes(int(signal_data["length"] / 8), byteorder="big"))
                except OverflowError:
                    signal_data["value"] = 0
                    chunks.append(signal_data["value"].to_bytes(int(signal_data["length"] / 8), byteorder="big"))
                    
                random_inc = randint(-5, 20)
                signal_data["value"] += random_inc
                if signal_data["value"] < 0:
                    signal_data["value"] *= -1
                
            chunks.append(DATAGRAM_EOF.to_bytes(1, byteorder="big"))

            out = b"".join(chunks)
            return out
        
        # for name, message in data["Messages"].items():
        #     chunks.append(DATAGRAM_SOF.to_bytes(2, byteorder="big"))
        #     chunks.append(message['id'].to_bytes(2, byteorder="big"))

        #     total_length = 0
        #     for signal_name, signal_data in message["signals"].items():
        #         total_length += signal_data["length"]

        #     if total_length % 8 == 0:
        #         total_length = (int) (total_length / 8)
        #     else:
        #         raise Exception("Invalid length")
                

        #     chunks.append(total_length.to_bytes(1, byteorder="big"))

        #     index = 0

        #     for signal_name, signal_data in message["signals"].items():
        #         if signal_data["length"] % 8 != 0:
        #             raise Exception("Invalid length")
                
        #         chunks.append(outputnums[index].to_bytes(int(signal_data["length"] / 8), byteorder="big"))
        #         index += 1
            
        #     chunks.append(DATAGRAM_EOF.to_bytes(1, byteorder="big"))

        # out = b"".join(chunks)
        # return out
