from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path
from typing import Any

import yaml


BOARD_ORDER = [
    "telemetry",
    "front_controller",
    "rear_controller",
    "steering",
]

NAME_OVERRIDES = {
    ("front_controller", "pedal"): "front_controller_pedal",
}


DBC_NAME_OVERRIDES = {
    ("front_controller", "pedal"): "front_controller_pedal_data",
    ("steering", "steering"): "steering_buttons",
}


def parse_dbc_message_ids(dbc_path: Path) -> dict[str, int]:
    """Parse BO_ entries from DBC and return message-name -> CAN-ID."""
    text = dbc_path.read_text(encoding="utf-8")
    message_ids: dict[str, int] = {}

    for raw_id, raw_name in re.findall(r"(?m)^BO_\s+(\d+)\s+([A-Za-z0-9_]+)\s*:", text):
        message_ids[raw_name] = int(raw_id)

    if not message_ids:
        raise ValueError(f"No BO_ message definitions found in DBC: {dbc_path}")

    return message_ids


def iter_board_files(cache_dir: Path) -> list[Path]:
    board_files = [p for p in cache_dir.glob("*.yaml") if p.is_file()]

    order_lookup = {name: idx for idx, name in enumerate(BOARD_ORDER)}

    return sorted(
        board_files,
        key=lambda p: (order_lookup.get(p.stem, len(order_lookup) + 1), p.stem),
    )


def flattened_signals(
    raw_signals: dict[str, Any],
) -> tuple[list[dict[str, int | str]], int]:
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
                    raise ValueError(
                        f"Unsupported flag config in '{signal_name}': {flag!r}"
                    )

                if not name:
                    raise ValueError(
                        f"Bitfield flag in '{signal_name}' is missing a name"
                    )

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


def resolve_dbc_name(board: str, raw_name: str, output_name: str) -> list[str]:
    """Return candidate DBC message names in priority order."""
    candidates = [
        DBC_NAME_OVERRIDES.get((board, raw_name)),
        output_name,
        raw_name,
        f"{board}_{raw_name}",
        f"{board}_{raw_name}_data",
    ]

    seen = set()
    resolved: list[str] = []
    for candidate in candidates:
        if candidate and candidate not in seen:
            seen.add(candidate)
            resolved.append(candidate)

    return resolved


def generate_global_messages(cache_dir: Path, dbc_path: Path) -> list[dict[str, Any]]:
    dbc_message_ids = parse_dbc_message_ids(dbc_path)

    messages: list[dict[str, Any]] = []
    for board_file in iter_board_files(cache_dir):
        board = board_file.stem
        if board == "system_can":
            continue

        board_data = yaml.safe_load(board_file.read_text(encoding="utf-8")) or {}
        board_messages = board_data.get("Messages", {})

        for raw_message_name, message_cfg in board_messages.items():
            output_name = output_message_name(board, str(raw_message_name))
            dbc_candidates = resolve_dbc_name(board, str(raw_message_name), output_name)
            dbc_name = next(
                (name for name in dbc_candidates if name in dbc_message_ids), None
            )

            if dbc_name is None:
                print(
                    "Warning: Skipping message without DBC ID: "
                    f"'{board}.{raw_message_name}' (output name '{output_name}'). "
                    f"Tried: {dbc_candidates}",
                    file=sys.stderr,
                )
                continue

            signals, total_bits = flattened_signals(message_cfg.get("signals", {}))
            message_id = dbc_message_ids[dbc_name]

            messages.append(
                {
                    "id": message_id,
                    "name": output_name,
                    "dlc": (total_bits + 7) // 8,
                    "signals": signals,
                }
            )

    if not messages:
        raise ValueError(
            "No messages were generated. Ensure the DBC contains BO_ entries for your board YAML messages."
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
        help="Directory containing fetched board YAML files",
    )
    parser.add_argument(
        "--dbc-path",
        type=Path,
        default=Path("can") / "fetched_cache" / "system_dbc.dbc",
        help="Path to DBC file that defines canonical CAN IDs",
    )
    parser.add_argument(
        "--output",
        type=Path,
        default=Path("can") / "global_can.yaml",
        help="Output path for generated global CAN YAML",
    )
    args = parser.parse_args()

    repo_root = Path(__file__).resolve().parents[2]
    cache_dir = (
        args.cache_dir if args.cache_dir.is_absolute() else repo_root / args.cache_dir
    )
    dbc_path = (
        args.dbc_path if args.dbc_path.is_absolute() else repo_root / args.dbc_path
    )
    output_path = args.output if args.output.is_absolute() else repo_root / args.output

    if not cache_dir.exists():
        raise FileNotFoundError(f"Cache directory not found: {cache_dir}")
    if not dbc_path.exists():
        raise FileNotFoundError(f"DBC file not found: {dbc_path}")

    messages = generate_global_messages(cache_dir, dbc_path)
    payload = {"messages": messages}

    output_path.parent.mkdir(parents=True, exist_ok=True)
    output_path.write_text(yaml.safe_dump(payload, sort_keys=False), encoding="utf-8")

    print(f"Generated {output_path} with {len(messages)} messages")


if __name__ == "__main__":
    main()
