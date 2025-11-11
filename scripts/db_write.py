from influxdb_client import InfluxDBClient, Point, WriteOptions
from influxdb_client.client.write_api import SYNCHRONOUS
from datetime import datetime, timezone

URL    = "http://localhost:8087"
TOKEN  = "local-token"
ORG    = "local-org"
BUCKET = "telemetry"

client = InfluxDBClient(url=URL, token=TOKEN, org=ORG)

write = client.write_api(write_options=SYNCHRONOUS)

def write_dict(name: str, data: dict, verbosity: bool):
    
    for field_name, info in data.items():
        if verbosity:
            print(f"{name} -> {field_name} = {info["value"]}")
        p = (Point(name)
            .field(field_name, info["value"])
            .time(datetime.now(timezone.utc)))
        write.write(bucket=BUCKET, org=ORG, record=p)