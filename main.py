from scripts.sim_serial import SimSerial
from scripts.can_sim import CanMessageSimulator
from scripts.decoder import Decoder
import time
import scripts.db_write as dbw

if __name__ == "__main__":
    sim = SimSerial()
    canSim = CanMessageSimulator()

    boards = ["front_controller", "imu", "rear_controller", "steering", "telemetry"]

    sim.open()

    for board in boards:
        sim.feed(canSim.gen_datagram(board))

    decoder = Decoder(ser=sim)

    while 1:
        decoder.read()

    # try:
    #     for i in range(50):
    #         dbw.write_point("test1", "Imu tilt", i)
    #         print(i)
    #         time.sleep(0.05)
    # finally:
    #     dbw.write.flush()
    #     dbw.client.close()