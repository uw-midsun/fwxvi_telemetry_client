from __future__ import annotations

import argparse
import re
from pathlib import Path
from typing import Any

import yaml


DEFAULT_ID_OFFSET = 4
DEFAULT_PRIORITY_BIT = 0x400
DEFAULT_DEVICE_MAP = {
	"telemetry": 0,
	"front_controller": 1,
	"rear_controller": 2,
	"imu": 3,
	"can_communication": 4,
	"steering": 5,
}

BOARD_ORDER = [
	"telemetry",
	"front_controller",
	"rear_controller",
	"steering",
]

NAME_OVERRIDES = {
	("front_controller", "pedal"): "front_controller_pedal",
}


def parse_system_can(system_can_path: Path) -> tuple[int, int, dict[str, int]]:
	"""Parse bit constants and device enum values from fetched system_can.py."""
	text = system_can_path.read_text(encoding="utf-8")

	id_offset_match = re.search(r"SYSTEM_CAN_MESSAGE_ID_OFFSET\s*=\s*(\d+)", text)
	priority_bit_match = re.search(r"SYSTEM_CAN_MESSAGE_PRIORITY_BIT\s*=\s*(0x[0-9A-Fa-f]+|\d+)", text)

	id_offset = int(id_offset_match.group(1)) if id_offset_match else DEFAULT_ID_OFFSET
	priority_bit = int(priority_bit_match.group(1), 0) if priority_bit_match else DEFAULT_PRIORITY_BIT

	device_map: dict[str, int] = {}
	for name, value in re.findall(r"SYSTEM_CAN_DEVICE_([A-Z_]+)\s*=\s*(\d+)", text):
		board = name.lower()
		if board == "num_system_can_devices":
			continue
		device_map[board] = int(value)

	if not device_map:
		device_map = dict(DEFAULT_DEVICE_MAP)

	return id_offset, priority_bit, device_map


def iter_board_files(cache_dir: Path) -> list[Path]:
	board_files = [
		p for p in cache_dir.glob("*.yaml") if p.is_file()
	]

	order_lookup = {name: idx for idx, name in enumerate(BOARD_ORDER)}

	return sorted(
		board_files,
		key=lambda p: (order_lookup.get(p.stem, len(order_lookup) + 1), p.stem),
	)


def flattened_signals(raw_signals: dict[str, Any]) -> tuple[list[dict[str, int | str]], int]:
	"""Expand message signal config into flat start_bit/length entries and total bit width."""
	result: list[dict[str, int | str]] = []
	bit_cursor = 0

	for signal_name, signal_cfg in raw_signals.items():
		signal_type = signal_cfg.get("type")

		if signal_type == "bitfield":
			bitfield_start = bit_cursor
			flags = signal_cfg.get("flags", [])
			for flag in flags:
				if isinstance(flag, str):
					name = flag
					length = 1
				elif isinstance(flag, dict):
					name = flag.get("name")
					length = int(flag.get("length", 1))
				else:
					raise ValueError(f"Unsupported flag config in '{signal_name}': {flag!r}")

				if not name:
					raise ValueError(f"Bitfield flag in '{signal_name}' is missing a name")

				result.append(
					{
						"name": str(name),
						"start_bit": bit_cursor,
						"length": length,
					}
				)
				bit_cursor += length

			declared_length = int(signal_cfg.get("length", bit_cursor - bitfield_start))
			bit_cursor = max(bit_cursor, bitfield_start + declared_length)
			continue

		if "length" not in signal_cfg:
			raise ValueError(f"Signal '{signal_name}' is missing required 'length'")

		length = int(signal_cfg["length"])
		result.append(
			{
				"name": str(signal_name),
				"start_bit": bit_cursor,
				"length": length,
			}
		)
		bit_cursor += length

	return result, bit_cursor


def output_message_name(board: str, raw_name: str) -> str:
	return NAME_OVERRIDES.get((board, raw_name), raw_name)


def generate_global_messages(cache_dir: Path) -> list[dict[str, Any]]:
	system_can_path = cache_dir / "system_can.py"
	id_offset, priority_bit, device_map = parse_system_can(system_can_path)

	messages: list[dict[str, Any]] = []
	for board_file in iter_board_files(cache_dir):
		board = board_file.stem
		if board == "system_can":
			continue

		if board not in device_map:
			raise ValueError(
				f"Board '{board}' from {board_file.name} not found in SystemCanDevice enum"
			)

		board_data = yaml.safe_load(board_file.read_text(encoding="utf-8")) or {}
		board_messages = board_data.get("Messages", {})

		for raw_message_name, message_cfg in board_messages.items():
			local_id = int(message_cfg["id"])
			critical = bool(message_cfg.get("critical", False))
			signals, total_bits = flattened_signals(message_cfg.get("signals", {}))

			message_id = (local_id << id_offset) + int(device_map[board])
			if not critical:
				message_id += priority_bit

			messages.append(
				{
					"id": message_id,
					"name": output_message_name(board, str(raw_message_name)),
					"dlc": (total_bits + 7) // 8,
					"signals": signals,
				}
			)

	return messages


def main() -> None:
	parser = argparse.ArgumentParser(
		description="Generate can/global_can.yaml from fetched_cache board YAML files."
	)
	parser.add_argument(
		"--cache-dir",
		type=Path,
		default=Path("can") / "fetched_cache",
		help="Directory containing fetched board YAML files and system_can.py",
	)
	parser.add_argument(
		"--output",
		type=Path,
		default=Path("can") / "global_can.yaml",
		help="Output path for generated global CAN YAML",
	)
	args = parser.parse_args()

	repo_root = Path(__file__).resolve().parents[2]
	cache_dir = args.cache_dir if args.cache_dir.is_absolute() else repo_root / args.cache_dir
	output_path = args.output if args.output.is_absolute() else repo_root / args.output

	if not cache_dir.exists():
		raise FileNotFoundError(f"Cache directory not found: {cache_dir}")

	messages = generate_global_messages(cache_dir)
	payload = {"messages": messages}

	output_path.parent.mkdir(parents=True, exist_ok=True)
	output_path.write_text(yaml.safe_dump(payload, sort_keys=False), encoding="utf-8")

	print(f"Generated {output_path} with {len(messages)} messages")


if __name__ == "__main__":
	main()
