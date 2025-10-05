from scripts.sim_serial import SimSerial
from scripts.can_sim import CanMessageSimulator

if __name__ == "__main__":
    sim = SimSerial()
    canSim = CanMessageSimulator()

    boards = ["front_controller", "imu", "rear_controller", "steering"]

    sim.open()

    for board in boards:
        sim.feed(canSim.gen_datagram(board))

    while 1:
        print("READING:", sim.read())