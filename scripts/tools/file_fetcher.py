import argparse
import base64
from pathlib import Path

import requests

OWNER = "uw-midsun"
REPO = "fwxvi"
DIRECTORY_PATH = "can/boards/"


boards = ["front_controller", "rear_controller", "steering", "telemetry"]


def parse_args():
    parser = argparse.ArgumentParser(
        description="Fetch CAN board files and tools from GitHub"
    )
    parser.add_argument(
        "-b",
        "--branch",
        nargs="?",
        const=None,
        default=None,
        help="Optional branch name to fetch files from (uses repo default branch if omitted)",
    )
    return parser.parse_args()


def fetch_file(source_path: str, destination_path: Path, branch: str | None = None):
    url = f"https://api.github.com/repos/{OWNER}/{REPO}/contents/{source_path}"
    params = {"ref": branch} if branch else None

    response = requests.get(url, params=params, timeout=30)
    response.raise_for_status()
    data = response.json()

    content = base64.b64decode(data["content"])
    destination_path.parent.mkdir(parents=True, exist_ok=True)
    with destination_path.open("wb") as output_file:
        output_file.write(content)


if __name__ == "__main__":
    args = parse_args()

    for board in boards:
        fetch_file(
            source_path=f"{DIRECTORY_PATH}{board}.yaml",
            destination_path=Path(f"./can/fetched_cache/{board}.yaml"),
            branch=args.branch,
        )

    fetch_file(
        source_path="can/tools/system_can.py",
        destination_path=Path("./can/fetched_cache/system_can.py"),
        branch=args.branch,
    )

    fetch_file(
        source_path="can/tools/system_dbc.dbc",
        destination_path=Path("./can/fetched_cache/system_dbc.dbc"),
        branch=args.branch,
    )
