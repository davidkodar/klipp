"""Real token cost of catalog slices through Claude Code (#688)."""
import json, subprocess, sys
sys.path.insert(0, sys.path[0])
from catalog import catalog
B = sys.path[0]
from bench import CWD
CWD.mkdir(parents=True, exist_ok=True)
cats = {v: catalog(f"{B}/bin/klipp-mcp-{v}") for v in ("pre", "post")}
def cost(tools, tag):
    tf = f"{B}/slice-{tag}.json"; json.dump(tools, open(tf, "w"))
    cfg = f"{B}/mcp-slice.json"
    json.dump({"mcpServers": {"klipp": {"command": "python3", "args": [f"{B}/fake_mcp.py"], "env": {"FAKE_TOOLS": tf}}}}, open(cfg, "w"))
    out = subprocess.run(["claude", "-p", "Do not call any tool. Reply with the single word OK.", "--model", "sonnet", "--mcp-config", cfg,
                          "--strict-mcp-config", "--setting-sources", "", "--tools", "", "--output-format", "json", "--no-session-persistence"],
                         cwd=CWD, capture_output=True, text=True, timeout=300).stdout
    u = json.loads(out)["usage"]
    return u["input_tokens"] + u.get("cache_creation_input_tokens", 0) + u.get("cache_read_input_tokens", 0) - BASE
BASE = 0
# What Claude Code's own prompt costs with an empty catalog: subtracted.
BASE = cost([], "none")
print("no tools", BASE, flush=True)
rows = {}
for v, tools in cats.items():
    big = {"klipp_apply", "klipp_plan"}
    rows[v] = {
        "all": cost(tools, f"{v}-all"),
        "apply+plan": cost([t for t in tools if t["name"] in big], f"{v}-ap"),
        "rest": cost([t for t in tools if t["name"] not in big], f"{v}-rest"),
        "rest_no_outputSchema": cost([{k: x for k, x in t.items() if k != "outputSchema"} for t in tools if t["name"] not in big], f"{v}-rest-noout"),
    }
    print(v, rows[v], flush=True)
json.dump(rows, open(f"{B}/slices.json", "w"), indent=1)
