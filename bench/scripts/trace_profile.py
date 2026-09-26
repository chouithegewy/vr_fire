"""Summarise a Bevy Chrome trace (TRACE_CHROME=<file>, `trace` feature) and build a flamegraph.

Streams the trace line by line (they are large: ~300 MB per second at 300 fps), rebuilds each
thread's span stack from the begin/end events, and writes:

  <out>.md       frames, busiest spans by self time per frame, per-thread busy time
  <out>.folded   collapsed stacks (thread;span;child... self-µs) for inferno-flamegraph
  <out>.svg      the flamegraph, if inferno-flamegraph is on PATH

Usage: python bench/scripts/trace_profile.py TRACE.json OUT_PREFIX [--skip-s 2] [--title T]
"""
import argparse, collections, json, re, shutil, subprocess, sys

NAME = re.compile(r'"name":"((?:[^"\\]|\\.)*)"')
PH = re.compile(r'"ph":"(.)"')
TID = re.compile(r'"tid":(\d+)')
TS = re.compile(r'"ts":([0-9.]+)')


def clean(raw: str) -> str:
    """`system: name="bevy_pbr::render::mesh::extract_meshes"` -> `extract_meshes (bevy_pbr)`."""
    s = raw.replace('\\"', '"')
    m = re.match(r'([a-zA-Z_ :]+?): name="(.*)"$', s)
    if m:
        kind, path = m.group(1), m.group(2)
        base = re.sub(r"<.*>", "<…>", path)
        parts = base.split("::")
        short = parts[-1] if len(parts) > 1 else base
        crate = parts[0] if len(parts) > 1 else ""
        s = f"{short} ({crate})" if kind.strip() in ("system", "system_commands") else f"{kind.strip()} {short}"
        if kind.strip() == "system_commands":
            s = "commands: " + s
    s = s.split(": query")[0]
    return s.replace(";", ",")[:90]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("trace")
    ap.add_argument("out")
    ap.add_argument("--skip-s", type=float, default=2.0, help="ignore the first N seconds after the first frame")
    ap.add_argument("--title", default="Bevy trace")
    a = ap.parse_args()

    threads = {}
    stacks = collections.defaultdict(list)  # tid -> [(name, start_ts, child_time)]
    self_us = collections.Counter()
    total_us = collections.Counter()
    calls = collections.Counter()
    folded = collections.Counter()
    busy = collections.Counter()  # tid -> top-level span time
    frames = 0
    first_frame = None
    t_min, t_max = None, None
    with open(a.trace, "r", encoding="utf-8", errors="replace") as f:
        for line in f:
            if '"thread_name"' in line:
                ev = json.loads(line.rstrip().rstrip(","))
                threads[ev["tid"]] = ev["args"]["name"]
                continue
            ph = PH.search(line)
            if not ph or ph.group(1) not in "BE":
                continue
            tid = int(TID.search(line).group(1))
            ts = float(TS.search(line).group(1))
            name = clean(NAME.search(line).group(1))
            st = stacks[tid]
            if ph.group(1) == "B":
                st.append([name, ts, 0.0])
                if name.startswith("queue_submit"):
                    if first_frame is None:
                        first_frame = ts
            else:
                if not st:
                    continue
                n, start, child = st.pop()
                dur = ts - start
                if st:
                    st[-1][2] += dur
                counting = first_frame is not None and start >= first_frame + a.skip_s * 1e6
                if not counting:
                    continue
                t_min = start if t_min is None else min(t_min, start)
                t_max = ts if t_max is None else max(t_max, ts)
                own = max(dur - child, 0.0)
                self_us[n] += own
                total_us[n] += dur
                calls[n] += 1
                if n.startswith("queue_submit"):
                    frames += 1
                if not st:
                    busy[tid] += dur
                path = ";".join([threads.get(tid, f"thread {tid}")] + [s[0] for s in st] + [n])
                folded[path] += own
    if not frames:
        sys.exit("no frames (queue_submit spans) in the measured window")
    wall_us = t_max - t_min
    per = lambda us: us / frames / 1000.0

    with open(a.out + ".folded", "w") as f:
        for path, us in folded.items():
            if us >= 1:
                f.write(f"{path} {int(us)}\n")
    lines = [f"# {a.title}", "", f"- Measured window: {wall_us / 1e6:.2f} s, {frames} frames ({frames / (wall_us / 1e6):.0f} fps under tracing; tracing itself slows frames)", ""]
    lines += ["## Busiest spans (self time)", "", "| ms / frame | % of wall | calls / frame | span |", "|---:|---:|---:|---|"]
    for n, us in self_us.most_common(30):
        lines.append(f"| {per(us):.3f} | {100 * us / wall_us:.1f} | {calls[n] / frames:.1f} | `{n}` |")
    lines += ["", "## Threads (top-level span time)", "", "| thread | busy ms / frame | busy % |", "|---|---:|---:|"]
    for tid, us in busy.most_common(12):
        lines.append(f"| {threads.get(tid, tid)} | {per(us):.3f} | {100 * us / wall_us:.1f} |")
    open(a.out + ".md", "w").write("\n".join(lines) + "\n")
    print("\n".join(lines[:40]))
    fg = shutil.which("inferno-flamegraph")
    if fg:
        with open(a.out + ".folded") as src, open(a.out + ".svg", "w") as dst:
            subprocess.run([fg, "--title", a.title, "--countname", "µs"], stdin=src, stdout=dst, check=True)
        print(f"flamegraph: {a.out}.svg")


if __name__ == "__main__":
    main()
