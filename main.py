from scripts.sim_serial import SimSerial
from scripts.can_sim import CanMessageSimulator
from scripts.decoder import Decoder
import time
import scripts.db_write as dbw
import json

if __name__ == "__main__":
    sim = SimSerial()
    canSim = CanMessageSimulator()

    boards = ["front_controller", "imu", "rear_controller", "steering", "telemetry"]

    sim.open()

    canSim.read_config(boards)
    

    decoder = Decoder(ser=sim)

    index = 0

    try:

        while 1:
            index += 1
            
            if index % 25 == 0:
                sim.feed(canSim.gen_datagram(), paced=True)
            decoder.read()
            if decoder.read() == True:
                dbw.write_dict(decoder.decoded_data)
    
    except KeyboardInterrupt:
        dbw.write.flush()
        dbw.client.close()
    
    # with open("decoded_data.json", "w") as file:
    #     json.dump(decoder.decoded_data, file, indent=1)

    #     dbw.write_dict(decoder.decoded_data)

    # try:
    #     for i in range(50):
    #         dbw.write_point("test1", "Imu tilt", i)
    #         print(i)
    #         time.sleep(0.05)
    # finally: