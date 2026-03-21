from digidevice import xbee

device = xbee.get_device()
try:
    device.open()

    def data_receive_callback(xb_msg):
        print("From %s >> %s" % (xb_msg.remote_device, xb_msg.data.decode()))

    device.add_data_received_callback(data_receive_callback)
    print("Waiting for data...\n")
    # Keep the program executing until a key is pressed
    input()
finally:
    if device.is_open():
        device.close()
