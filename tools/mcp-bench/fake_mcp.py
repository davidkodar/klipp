"""An MCP server that serves a saved tools list and nothing else, to
measure what any slice of a catalog costs an agent in tokens."""
import json, os, sys
tools = json.load(open(os.environ["FAKE_TOOLS"]))
for line in sys.stdin:
    req = json.loads(line)
    if "id" not in req:
        continue
    m = req["method"]
    if m == "initialize":
        res = {"protocolVersion": req["params"].get("protocolVersion", "2025-06-18"), "capabilities": {"tools": {}}, "serverInfo": {"name": "klipp", "version": "0"}}
    elif m == "tools/list":
        res = {"tools": tools}
    else:
        res = {}
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": req["id"], "result": res}) + "\n"); sys.stdout.flush()
