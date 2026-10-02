"""MCP catalog benchmark (#688): the same agent, prompts and fixtures against
the klipp-mcp catalog from before #669 (pre) and after it (post).

Each run resets the test Klipp (a new 25 fps project, the three fixtures
imported, the scenario's starting timeline built over the socket), runs one
headless `claude -p` with only that MCP server, then scores the final
timeline over the socket against the scenario's expected clips, to the
frame.

    python3 bench.py run [--reps N] [--only name,name] [--variants pre,post]
    python3 bench.py summary
"""
import argparse, json, os, subprocess, sys, time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import k  # noqa: E402

FPS = 25
FIX = HERE / "fixtures"
RESULTS = HERE / "results.jsonl"
# Scratch: the projects each run makes, and an empty directory the agent
# runs in, so no project instructions reach it.
SCRATCH = Path(os.environ.get("KLIPP_BENCH_SCRATCH", "/tmp/klipp-mcp-bench"))
PROJECTS = SCRATCH / "projects"
CWD = SCRATCH / "cwd"
MODEL = os.environ.get("BENCH_MODEL", "sonnet")

PREAMBLE = ("Klipp is open with a project whose media pool holds A.mp4, B.mp4 and C.mp4 "
            "(25 fps, 20 s each). Use the Klipp tools. Do only what is asked, then stop. ")


def ok(r):
    if not r.get("ok"):
        raise RuntimeError(r.get("error"))
    return r["result"]


def reset(setup):
    """A fresh project with the fixtures, and the starting timeline."""
    PROJECTS.mkdir(parents=True, exist_ok=True)
    ok(k.call({"method": "new_project", "name": f"bench-{int(time.time() * 1000)}", "location": str(PROJECTS),
               "fps": FPS, "width": 640, "height": 360, "discard_changes": True}))
    ids = {}
    for c in "ABC":
        r = ok(k.call({"method": "import_media", "path": str(FIX / f"{c}.mp4"), "analyze": False}))
        ids[c] = r["media_id"]
    ops = []
    for (c, track, at, sin, sout) in setup:
        ops.append({"op": "place", "media_id": ids[c], "track": track, "at": {"frame": at},
                    "source_in": sin / FPS, "source_out": sout / FPS})
    if ops:
        ok(k.call({"method": "apply", "ops": ops}))
    return ids


def clips():
    """Every clip on the timeline: (track kind, track index, file, tl in/out, src in/out) in frames."""
    r = ok(k.call({"method": "list_clips", "full": True}))
    out = []
    for t in r["tracks"]:
        for c in t["clips"]:
            name = (c.get("media") or {}).get("filename", c.get("kind"))
            out.append({"kind": t["kind"], "track": t["index"], "track_name": t["name"], "file": name,
                        "tl": [c["timeline_in_frame"], c["timeline_out_frame"]],
                        "src": [round(c.get("source_in_secs", 0) * FPS), round(c.get("source_out_secs", 0) * FPS)]})
    return out, r


def video(cs):
    return sorted([(c["file"], c["tl"][0], c["tl"][1], c["src"][0], c["src"][1]) for c in cs if c["kind"] == "Video"], key=lambda x: (x[1], x[0]))


def audio(cs):
    return sorted([(c["file"], c["tl"][0], c["tl"][1], c["src"][0], c["src"][1]) for c in cs if c["kind"] == "Audio"], key=lambda x: (x[1], x[0]))


def same_video(expected):
    """Score: the video clips are exactly these (file, tl in, tl out, src in, src out), frames."""
    def score(cs, _raw):
        got = video(cs)
        exp = sorted(expected, key=lambda x: (x[1], x[0]))
        why = [] if got == exp else [f"video {got} != expected {exp}"]
        # Sound follows its picture unless the scenario says otherwise.
        if audio(cs) != exp:
            why.append(f"(note) audio {audio(cs)}")
        return (got == exp), why
    return score


def scenario_asm2(cs, _raw):
    v = video(cs)
    want = sorted([("A.mp4", 0, 150, 0, 150), ("C.mp4", 50, 100, 0, 50)], key=lambda x: (x[1], x[0]))
    tracks = {c["file"]: c["track"] for c in cs if c["kind"] == "Video"}
    c_above = tracks.get("C.mp4", 99) < tracks.get("A.mp4", -1)
    no_c_sound = not any(c["file"] == "C.mp4" for c in cs if c["kind"] == "Audio")
    why = []
    if v != want:
        why.append(f"video {v} != {want}")
    if not c_above:
        why.append(f"C not on a track above A: {tracks}")
    if not no_c_sound:
        why.append("C's sound was placed")
    return not why, why


def saved_transitions():
    """The transitions on each track, from the project saved now: no socket
    read shows them (the timeline view leaves them out)."""
    path = ok(k.call({"method": "save_project"}))["path"]
    d = json.load(open(Path(path) / "project.json"))
    seq = d["sequences"][0] if len(d["sequences"]) == 1 else next(s for s in d["sequences"] if s["id"] == d["active_sequence_id"])
    return [(t["kind"], [(x["kind"], round(x["duration"]["value"] * FPS / x["duration"]["rate"])) for x in t["transitions"]]) for t in seq["tracks"]]


def scenario_asm3(cs, raw):
    v = video(cs)
    want = [("B.mp4", 0, 75, 50, 125), ("A.mp4", 75, 150, 50, 125)]
    trs = saved_transitions()
    has_dissolve = any(kind == "Video" and any(k_ == "CrossDissolve" and f == 12 for k_, f in xs) for kind, xs in trs)
    why = []
    if v != want:
        why.append(f"video {v} != {want}")
    if not has_dissolve:
        why.append(f"no 12-frame cross dissolve on the video track: {trs}")
    return not why, why


def scenario_marker(cs, raw):
    r = ok(k.call({"method": "list_markers"}))
    ms = r.get("markers", r if isinstance(r, list) else [])
    hit = [m for m in ms if str(m.get("name", m.get("label", ""))).strip().lower() == "check"]
    why = []
    if not hit:
        why.append(f"no marker named Check: {json.dumps(ms)[:300]}")
    else:
        secs = hit[0].get("time_secs", hit[0].get("secs", hit[0].get("at_secs")))
        frame = hit[0].get("frame", round(secs * FPS) if secs is not None else None)
        if frame != 80:
            why.append(f"marker at frame {frame}, want 80 ({hit[0]})")
    if video(cs) != [("A.mp4", 0, 250, 0, 250)]:
        why.append(f"timeline changed: {video(cs)}")
    return not why, why


def scenario_captions(cs, raw):
    tracks = [(t["kind"], t["name"]) for t in raw["tracks"]]
    want = [t for t in tracks if t[0].lower() in ("subtitle", "subtitles") and t[1] == "Subs EN"]
    return bool(want), ([] if want else [f"tracks {tracks}"])


AB = [("A", "video", 0, 0, 200), ("B", "video", 200, 0, 200)]
SCENARIOS = [
    # Frame-accurate cuts.
    {"name": "cut_head", "kind": "cut", "setup": [],
     "prompt": "Put the first 4 seconds and 12 frames of A.mp4 on the video track at the start of the timeline.",
     "score": same_video([("A.mp4", 0, 112, 0, 112)])},
    {"name": "cut_out_range", "kind": "cut", "setup": [("A", "video", 0, 0, 250)],
     "prompt": "Remove timeline frames 100 through 149 (inclusive) and close the gap.",
     "score": same_video([("A.mp4", 0, 100, 0, 100), ("A.mp4", 100, 200, 150, 250)])},
    {"name": "cut_split_ripple", "kind": "cut", "setup": AB,
     "prompt": "Cut the first clip at timeline timecode 00:00:05:10, delete the part of it after the cut, and move everything after it left so nothing is left empty.",
     "score": same_video([("A.mp4", 0, 135, 0, 135), ("B.mp4", 135, 335, 0, 200)])},
    # Trims.
    {"name": "trim_tail", "kind": "trim", "setup": AB,
     "prompt": "Trim 12 frames off the end of the first clip, and pull the second clip back so there is no gap.",
     "score": same_video([("A.mp4", 0, 188, 0, 188), ("B.mp4", 188, 388, 0, 200)])},
    {"name": "trim_head", "kind": "trim", "setup": AB,
     "prompt": "Trim the start of the second clip so it begins at frame 25 of its source file, without leaving a gap after the first clip.",
     "score": same_video([("A.mp4", 0, 200, 0, 200), ("B.mp4", 200, 375, 25, 200)])},
    {"name": "trim_slip", "kind": "trim", "setup": AB,
     "prompt": "Slip the second clip so it shows 10 frames later in its source, without moving it on the timeline or changing its length.",
     "score": same_video([("A.mp4", 0, 200, 0, 200), ("B.mp4", 200, 400, 10, 210)])},
    # Short assemblies.
    {"name": "asm_three", "kind": "assembly", "setup": [],
     "prompt": "Assemble on the video track, back to back from the start: A.mp4 frames 0 to 49, then C.mp4 frames 100 to 174, then B.mp4 frames 25 to 99 (all ranges inclusive).",
     "score": same_video([("A.mp4", 0, 50, 0, 50), ("C.mp4", 50, 125, 100, 175), ("B.mp4", 125, 200, 25, 100)])},
    {"name": "asm_overlay", "kind": "assembly", "setup": [],
     "prompt": "Put the first 6 seconds of A.mp4 on the video track at the start. On a second video track above it, put the first 2 seconds of C.mp4 from timeline 2 s to 4 s, without C's sound.",
     "score": scenario_asm2},
    {"name": "asm_dissolve", "kind": "assembly", "setup": [],
     "prompt": "Assemble seconds 2 to 5 of B.mp4 and then seconds 2 to 5 of A.mp4, back to back from the start, with a 12-frame cross dissolve between them.",
     "score": scenario_asm3},
    # Less common tool selection, and a rule that lives in an input field after #669.
    {"name": "marker", "kind": "tool-choice", "setup": [("A", "video", 0, 0, 250)],
     "prompt": "Add a timeline marker named Check at timecode 00:00:03:05.",
     "score": scenario_marker},
    {"name": "captions_track", "kind": "field-guidance", "setup": [],
     "prompt": "Add a captions track named Subs EN.",
     "score": scenario_captions},
]


def mcp_config(variant):
    cfg = HERE / f"mcp-{variant}.json"
    cfg.write_text(json.dumps({"mcpServers": {"klipp": {
        "command": str(HERE / "bin" / f"klipp-mcp-{variant}"),
        "env": {"KLIPP_CONTROL_SOCKET": k.SOCK}}}}))
    return cfg


def run_agent(variant, prompt):
    CWD.mkdir(parents=True, exist_ok=True)
    cmd = ["claude", "-p", PREAMBLE + prompt, "--model", MODEL,
           "--mcp-config", str(mcp_config(variant)), "--strict-mcp-config",
           "--setting-sources", "", "--tools", "", "--allowedTools", "mcp__klipp",
           "--output-format", "stream-json", "--verbose", "--no-session-persistence",
           "--max-turns", "40"]
    t = time.time()
    p = subprocess.run(cmd, cwd=CWD, capture_output=True, text=True, timeout=900)
    wall = time.time() - t
    calls, errors, names, final = 0, 0, [], {}
    for line in p.stdout.splitlines():
        try:
            ev = json.loads(line)
        except json.JSONDecodeError:
            continue
        if ev.get("type") == "assistant":
            for b in ev["message"].get("content", []):
                if b.get("type") == "tool_use":
                    calls += 1
                    names.append(b["name"].replace("mcp__klipp__", ""))
        elif ev.get("type") == "user":
            for b in (ev.get("message", {}).get("content") or []):
                if isinstance(b, dict) and b.get("type") == "tool_result" and b.get("is_error"):
                    errors += 1
        elif ev.get("type") == "result":
            final = ev
    u = final.get("usage", {})
    return {"wall_s": round(wall, 1), "duration_ms": final.get("duration_ms"), "turns": final.get("num_turns"),
            "cost_usd": final.get("total_cost_usd"), "tool_calls": calls, "tool_errors": errors, "tools": names,
            "input_tokens": u.get("input_tokens"), "cache_creation": u.get("cache_creation_input_tokens"),
            "cache_read": u.get("cache_read_input_tokens"), "output_tokens": u.get("output_tokens"),
            "subtype": final.get("subtype"), "answer": (final.get("result") or "")[:400],
            "stderr": p.stderr[-300:] if p.returncode else ""}


def cmd_run(a):
    only = set(a.only.split(",")) if a.only else None
    variants = a.variants.split(",")
    for rep in range(a.reps):
        for s in SCENARIOS:
            if only and s["name"] not in only:
                continue
            # Alternate which variant goes first, so drift does not favour one.
            order = variants if (rep + SCENARIOS.index(s)) % 2 == 0 else variants[::-1]
            for v in order:
                reset(s["setup"])
                r = run_agent(v, s["prompt"])
                cs, raw = clips()
                try:
                    passed, why = s["score"](cs, raw)
                except Exception as e:  # a scoring read that failed is a failure, said
                    passed, why = False, [f"scoring error: {e}"]
                row = {"scenario": s["name"], "kind": s["kind"], "variant": v, "rep": rep, "model": MODEL,
                       "correct": passed, "why": why, "final_video": video(cs), **r}
                with RESULTS.open("a") as f:
                    f.write(json.dumps(row) + "\n")
                print(f"{s['name']:18} {v:4} rep{rep} {'PASS' if passed else 'FAIL'} calls={r['tool_calls']} err={r['tool_errors']} "
                      f"turns={r['turns']} {r['wall_s']}s ${r['cost_usd']} {'; '.join(why)[:160]}", flush=True)


def cmd_summary(_a):
    rows = [json.loads(l) for l in RESULTS.open()]
    by = {}
    for r in rows:
        by.setdefault(r["variant"], []).append(r)
    def avg(xs):
        xs = [x for x in xs if x is not None]
        return sum(xs) / len(xs) if xs else float("nan")
    for v, rs in sorted(by.items()):
        n = len(rs)
        print(f"{v}: runs {n}, correct {sum(r['correct'] for r in rs)}/{n}, "
              f"calls {avg([r['tool_calls'] for r in rs]):.1f}, errors {avg([r['tool_errors'] for r in rs]):.2f}, "
              f"turns {avg([r['turns'] for r in rs]):.1f}, wall {avg([r['wall_s'] for r in rs]):.0f}s, "
              f"input+cache {avg([(r['input_tokens'] or 0) + (r['cache_creation'] or 0) + (r['cache_read'] or 0) for r in rs]):.0f}, "
              f"output {avg([r['output_tokens'] for r in rs]):.0f}, cost ${avg([r['cost_usd'] for r in rs]):.3f}")
    print()
    for s in SCENARIOS:
        line = [f"{s['name']:18}"]
        for v in sorted(by):
            rs = [r for r in by[v] if r["scenario"] == s["name"]]
            if rs:
                line.append(f"{v} {sum(r['correct'] for r in rs)}/{len(rs)} calls {avg([r['tool_calls'] for r in rs]):.1f}")
        print("  ".join(line))


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    sp = ap.add_subparsers(dest="cmd", required=True)
    r = sp.add_parser("run")
    r.add_argument("--reps", type=int, default=1)
    r.add_argument("--only")
    r.add_argument("--variants", default="pre,post")
    sp.add_parser("summary")
    a = ap.parse_args()
    {"run": cmd_run, "summary": cmd_summary}[a.cmd](a)
