"""Talk to the test Klipp's control socket."""
import json, os, socket, sys
# The test Klipp's socket (run_klipp.sh starts it there): never the
# socket of the Klipp you are editing in.
SOCK = os.environ.get("KLIPP_BENCH_SOCKET", os.path.join(os.environ.get("XDG_RUNTIME_DIR", "/tmp"), "klipp-bench.sock"))
def call(req, timeout=120):
    s = socket.socket(socket.AF_UNIX); s.settimeout(timeout)
    s.connect(SOCK)
    s.sendall((json.dumps(req) + "\n").encode())
    buf = b""
    while not buf.endswith(b"\n"):
        c = s.recv(65536)
        if not c: break
        buf += c
    return json.loads(buf)
if __name__ == "__main__":
    print(json.dumps(call(json.loads(sys.argv[1])))[:int(sys.argv[2]) if len(sys.argv) > 2 else 3000])
