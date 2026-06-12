import argparse
from pathlib import Path
from scripts.decoder import Decoder
import scripts.db_write as dbw
from scripts.sim.sim_from_log import SimFromLog


def parse_can_id(value: str) -> int:
    try:
        can_id = int(value, 0)
    except ValueError as exc:
        raise argparse.ArgumentTypeError(
            f"Invalid CAN ID '{value}'. Use decimal (123) or hex (0x7B)."
        ) from exc

    if can_id < 0:
        raise argparse.ArgumentTypeError("CAN ID must be non-negative.")

    return can_id


def parse_args():
    parser = argparse.ArgumentParser(
        description="Telemetry decoder entry point for one CAN message ID"
    )
    parser.add_argument(
        "signal_id",
        type=parse_can_id,
        help="CAN message ID to capture (decimal or 0x-prefixed hex).",
    )
    parser.add_argument(
        "-m",
        "--mode",
        choices=["serial", "sim"],
        default="serial",
        help="Input source mode. Defaults to serial.",
    )
    parser.add_argument(
        "--log-file",
        help=".log file name from the logs folder (required for sim mode)",
    )
    parser.add_argument(
        "-d",
        "--dbw",
        action=argparse.BooleanOptionalAction,
        default=False,
        help="Enable or disable writing decoded values to the database.",
    )
    parser.add_argument(
        "--decoder_debug",
        action=argparse.BooleanOptionalAction,
        default=False,
        help="Enable or disable decoder debug mode",
    )
    return parser.parse_args()


def build_decoder(args):
    if args.mode == "serial":
        return Decoder(
            port="COM9",
            baudrate=230400,
            ser=None,
            debug_mode=args.decoder_debug,
            filter_id=args.signal_id,
        )

    if not args.log_file:
        raise ValueError("--log-file is required when --mode sim is used")

    log_path = Path("logs") / args.log_file
    if not log_path.is_file() or log_path.suffix.lower() != ".log":
        raise ValueError(f"Log file not found or invalid: {log_path}")

    sim_ser = SimFromLog(log_path=log_path, baudrate=230400)
    sim_ser.open()
    return Decoder(port="SIM", baudrate=230400, ser=sim_ser, filter_id=args.signal_id)


if __name__ == "__main__":
    args = parse_args()
    decoder = build_decoder(args)

    if args.dbw:
        decoder.enable_write_to_db()
    else:
        decoder.disable_write_to_db()

    try:
        while 1:
            has_data = decoder.read()
            if args.mode == "sim" and not has_data:
                break
    except KeyboardInterrupt:
        print(decoder.decoded_data)
    finally:
        decoder.close()
        if args.mode == "sim" and hasattr(decoder.ser, "close"):
            decoder.ser.close()
        dbw.write.flush()
        dbw.client.close()
