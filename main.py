from scripts.sim_serial import SimSerial
from scripts.can_sim import CanMessageSimulator

if __name__ == "__main__":
    sim = SimSerial()
    canSim = CanMessageSimulator()

    print(canSim.send_datagram("imu").hex())



    # sim.open()
    # sim.feed(b"HELLO,123")

    # while 1:
    #     print("READING:", sim.read())