import requests
import base64

OWNER = "uw-midsun"
REPO = "fwxvi"
DIRECTORY_PATH = "can/boards/"


boards = ["front_controller", "rear_controller", "steering", "telemetry"]

for board in boards:
    url = f"https://api.github.com/repos/{OWNER}/{REPO}/contents/{DIRECTORY_PATH}{board}.yaml"

    r = requests.get(url)
    data = r.json()

    content = base64.b64decode(data["content"])
    with open(f"./can/fetched_cache/{board}.yaml", "wb") as f:
        f.write(content)

url = f"https://api.github.com/repos/{OWNER}/{REPO}/contents/can/tools/system_can.py"

r = requests.get(url)
data = r.json()

content = base64.b64decode(data["content"])
with open("./can/fetched_cache/system_can.py", "wb") as f:
    f.write(content)
url = f"https://api.github.com/repos/{OWNER}/{REPO}/contents/can/tools/system_dbc.dbc"

r = requests.get(url)
data = r.json()

content = base64.b64decode(data["content"])
with open("./can/fetched_cache/system_dbc.dbc", "wb") as f:
    f.write(content)
