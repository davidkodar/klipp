"""The tools/list catalog of an MCP build: bytes, tools, and a rough token
estimate (bytes/4; the agent runs measure the real cost)."""
import json, subprocess, sys

def catalog(exe):
    msgs = [
        {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "bench", "version": "1"}}},
        {"jsonrpc": "2.0", "method": "notifications/initialized"},
        {"jsonrpc": "2.0", "id": 2, "method": "tools/list"},
    ]
    p = subprocess.run([exe], input="".join(json.dumps(m) + "\n" for m in msgs), capture_output=True, text=True, timeout=30,
                       env={"KLIPP_CONTROL_SOCKET": "/nonexistent", "PATH": "/usr/bin"})
    for line in p.stdout.splitlines():
        r = json.loads(line)
        if r.get("id") == 2:
            return r["result"]["tools"]
    raise SystemExit(f"no tools/list reply from {exe}: {p.stderr[-500:]}")

if __name__ == "__main__":
    out = {}
    for name in sys.argv[1:]:
        tools = catalog(name)
        raw = json.dumps(tools, separators=(",", ":"))
        out[name.rsplit("-", 1)[-1]] = {"tools": len(tools), "bytes": len(raw), "est_tokens": len(raw) // 4,
                                         "largest": sorted(((len(json.dumps(t)), t["name"]) for t in tools), reverse=True)[:5]}
    print(json.dumps(out, indent=1))
