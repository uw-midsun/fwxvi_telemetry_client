from scripts.sim_serial import SimSerial
from scripts.can_sim import CanMessageSimulator
from scripts.decoder import Decoder

if __name__ == "__main__":
    sim = SimSerial()
    canSim = CanMessageSimulator()

    boards = ["front_controller", "imu", "rear_controller", "steering"]

    sim.open()

    for board in boards:
        sim.feed(canSim.gen_datagram(board))

    decoder = Decoder(ser=sim)

    while 1:
        decoder.read()