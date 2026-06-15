import json
import os
import urllib.request

url = os.environ["ALCHEMY_HTTP"]
FACTORY = "0x420DD381b31aEf6683db6B902084cB0FFECe40Da"

payload = {
    "jsonrpc": "2.0",
    "method": "eth_getCode",
    "params": [FACTORY, "latest"],
    "id": 1
}
req = urllib.request.Request(
    url, 
    data=json.dumps(payload).encode('utf-8'), 
    headers={'Content-Type': 'application/json'}
)
with urllib.request.urlopen(req) as response:
    res = json.loads(response.read().decode('utf-8'))
    print("Code length:", len(res.get('result', '')))
    print("Code prefix:", res.get('result', '')[:100])
