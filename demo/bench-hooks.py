"""Compare the per-call cost of memsaga hook invocations between two binaries.

Usage: python3 demo/bench-hooks.py <old-binary> <new-binary> [runs]
       (build each with `cargo build --release`, for example the base branch in a
       `git worktree` and this one here, and run demo/setup.sh first)

Runs inside the demo environment built by memsaga's demo/setup.sh
(/tmp/memsaga-demo, no remote, so no network) and never touches a real data
repository. Each case runs the two binaries interleaved (old, new, old, ...)
so drift in machine load hits both alike. Wall time per call comes from
perf_counter; CPU time per call (user + sys of the child) from
resource.getrusage(RUSAGE_CHILDREN) deltas. Prints the median and p90.
"""
import json
import os
import re
import resource
import statistics
import subprocess
import sys
import time

DEMO = os.path.realpath("/tmp/memsaga-demo")
HOME = f"{DEMO}/home"
ENV = dict(
    os.environ,
    HOME=HOME,
    CLAUDE_CONFIG_DIR=f"{HOME}/.claude",
    MEMSAGA_CONFIG=f"{HOME}/.config/memsaga/config.toml",
    MEMSAGA_NO_GITHUB_API="1",
    GIT_CONFIG_GLOBAL=f"{DEMO}/gitconfig",
)
for k in ("MEMSAGA_REPO", "MEMSAGA_ROOTS"):
    ENV.pop(k, None)

OUT_OF_SCOPE = f"{HOME}/git/webapp/README.md"
CASES = {
    "PreToolUse Read, file out of scope": {
        "hook_event_name": "PreToolUse", "session_id": "bench", "cwd": f"{HOME}/git/webapp",
        "tool_name": "Read", "tool_input": {"file_path": OUT_OF_SCOPE},
    },
    "PostToolUse Write, file out of scope": {
        "hook_event_name": "PostToolUse", "session_id": "bench", "cwd": f"{HOME}/git/webapp",
        "tool_name": "Write", "tool_input": {"file_path": OUT_OF_SCOPE},
    },
    "Stop, nothing changed": {
        "hook_event_name": "Stop", "session_id": "bench", "cwd": f"{HOME}/git/webapp",
    },
    "Stop, one memory file changed (sweep commits)": {
        "hook_event_name": "Stop", "session_id": "bench", "cwd": f"{HOME}/git/webapp",
    },
}

# Cases that need a change before every call (made outside the timing).
WEBAPP_SLUG = re.sub(r"[^A-Za-z0-9]", "-", f"{HOME}/git/webapp")  # Claude Code's project name
CHANGED_FILE = f"{HOME}/.claude/projects/{WEBAPP_SLUG}/memory/bench.md"
COUNTER = [0]


def prepare(name):
    if "file changed" in name:
        COUNTER[0] += 1
        os.makedirs(os.path.dirname(CHANGED_FILE), exist_ok=True)
        with open(CHANGED_FILE, "w") as f:
            f.write(f"bench run {COUNTER[0]}\n")
        time.sleep(0.01)  # a newer mtime than the last sweep marker


def one(binary, payload):
    before = resource.getrusage(resource.RUSAGE_CHILDREN)
    t0 = time.perf_counter()
    done = subprocess.run([binary, "hook"], input=payload, env=ENV, capture_output=True, check=False)
    wall = time.perf_counter() - t0
    after = resource.getrusage(resource.RUSAGE_CHILDREN)
    cpu = (after.ru_utime - before.ru_utime) + (after.ru_stime - before.ru_stime)
    return wall * 1000, cpu * 1000, done.returncode


def head():
    out = subprocess.run(["git", "-C", f"{HOME}/git/memories", "rev-parse", "HEAD"],
                         capture_output=True, text=True, env=ENV, check=False)
    return out.stdout.strip()


def p90(xs):
    return sorted(xs)[int(len(xs) * 0.9) - 1]


def main():
    old, new = sys.argv[1], sys.argv[2]
    runs = int(sys.argv[3]) if len(sys.argv) > 3 else 60
    if not os.path.isdir(f"{HOME}/git/memories/.git/memsaga"):
        sys.exit("demo environment missing: run memsaga's demo/setup.sh first")
    print(f"{runs} runs per binary per case, interleaved; times in ms (median / p90)")
    for name, case in CASES.items():
        payload = json.dumps(case).encode()
        for b in (old, new):  # warm-up: page cache, dyld
            prepare(name)
            one(b, payload)
        res = {old: ([], []), new: ([], [])}
        must_commit = "file changed" in name
        rejected = {old: 0, new: 0}
        for _ in range(runs):
            for b in (old, new):
                prepare(name)
                before = head() if must_commit else None
                w, c, code = one(b, payload)
                # A hook exits 0 even when its sweep failed: only a call that did
                # what the case names counts (checked outside the timing).
                if code != 0 or (must_commit and head() == before):
                    rejected[b] += 1
                    continue
                res[b][0].append(w)
                res[b][1].append(c)
        print(f"\n{name}")
        for label, b in (("old", old), ("new", new)):
            if rejected[b]:
                print(f"  {label}: {rejected[b]} call(s) left out: nonzero exit or no commit")
            if not res[b][0]:
                sys.exit(f"  {label}: no valid call; is the demo environment intact?")
        for label, b in (("old", old), ("new", new)):
            w, c = res[b]
            print(f"  {label}: wall {statistics.median(w):6.2f} / {p90(w):6.2f}   "
                  f"cpu {statistics.median(c):6.2f} / {p90(c):6.2f}")
        dw = statistics.median(res[new][0]) - statistics.median(res[old][0])
        dc = statistics.median(res[new][1]) - statistics.median(res[old][1])
        print(f"  new - old (medians): wall {dw:+.2f}   cpu {dc:+.2f}")


if __name__ == "__main__":
    main()
