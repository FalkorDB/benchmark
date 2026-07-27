#!/usr/bin/env python3
"""Phase 7 §6.5 evidence: corpus/topology partition analysis for a recorded bundle (E3/E4).

Reproduces the two engine-independent figures behind the "recorded write replay stays C=1"
decision in `docs/design/synthetic-cover-writes-phase7.md` (phasing item 5), from a bundle's
on-disk files only (no server):

- `edges <bundle-dir>`  — E3, topology: how many recorded base-graph edges cross a W-way
  contiguous node-id band partition. Bands split the 1-based id range `1..=N` into W equal
  bands of `N/W` ids: `band(id) = (id - 1) // (N / W)`; an edge crosses iff
  `band(src) != band(dst)`.
- `corpus <bundle-dir>` — E4, collisions: per write-op corpus (the K=256 rendered commands),
  how commands collide on the node ids they target. Each command *touches* the node ids in
  its rendered params — node-targeted shapes touch `{id}`, edge-MERGE shapes touch
  `{from, to}`; `single_edge_update` (rand()-targeted, no param target) is skipped, and any
  other command without a recognizable target is an error (fail closed — a format change
  must not silently drop collision evidence). Reported per op, for a W-way contiguous split
  of the corpus into `K/W`-command chunks (`worker(seq) = seq // (K / W)`):
    - `repeat-touches`: touches of an id already touched by an earlier command,
      `sum(touches(id) - 1)` over ids touched >= 2 times;
    - `cross-worker`: the subset of repeat touches landing on a *different* worker than an
      earlier touch of the same id — mutations of one node racing across workers;
    - for edge-MERGE shapes, whether any directed `(from, to)` pair repeats within the
      corpus (in the seed-42 bundles it never does — every MERGE targets a distinct edge).

W must be >= 1 and divide the node count (`edges`) / corpus size (`corpus`) exactly, so
bands/chunks are equal — the split the figures are defined over; anything else is rejected.

Usage:
    python3 scripts/synthetic_p65_partition_analysis.py edges  <bundle-dir> [workers]
    python3 scripts/synthetic_p65_partition_analysis.py corpus <bundle-dir> [workers]

The bundles themselves are reproducible offline: `synthetic record --seed <s> --nodes <n>
--edges <e> …` is deterministic (same seed + same tool build ⇒ identical bundle).
"""

import json
import re
import sys
from pathlib import Path
from typing import Dict, List, Optional, Tuple

EDGE_PAIR = re.compile(r"\{src:(\d+),dst:(\d+)")
PARAMS = re.compile(r"(\w+)\s*=\s*(\d+)")

# Ops with no deterministic param target (the server picks the row via rand()) — the only
# corpora the collision analysis may skip; anything else unparsable is an error.
RAND_TARGETED = {"single_edge_update"}


def split_size(total: int, workers: int, what: str) -> int:
    """The per-worker share of an exact W-way contiguous split, validating the split exists."""
    if workers < 1:
        sys.exit(f"workers must be >= 1, got {workers}")
    if total == 0 or total % workers != 0:
        sys.exit(
            f"{what} ({total}) must be a non-zero multiple of workers ({workers}) — the "
            f"figures are defined over equal contiguous bands/chunks"
        )
    return total // workers


def load_edges(bundle: Path) -> List[Tuple[int, int]]:
    pairs = []
    with open(bundle / "graph.jsonl") as fh:
        for line in fh:
            record = json.loads(line)
            if record["phase"] == "edges":
                pairs += [(int(a), int(b)) for a, b in EDGE_PAIR.findall(record["cypher"])]
    return pairs


def edges_report(bundle: Path, workers: int) -> None:
    pairs = load_edges(bundle)
    with open(bundle / "manifest.json") as fh:
        nodes = json.load(fh)["dataset"]["nodes"]
    band_size = split_size(nodes, workers, "node count")
    if not pairs:
        sys.exit("no edges found in the bundle's graph.jsonl edges phase")

    def band(node_id: int) -> int:
        return (node_id - 1) // band_size

    crossing = sum(1 for src, dst in pairs if band(src) != band(dst))
    print(f"nodes={nodes} edges={len(pairs)} workers={workers} band_size={band_size}")
    print(f"band(id) = (id - 1) // {band_size}  (ids are 1-based)")
    print(f"crossing edges: {crossing} / {len(pairs)} ({100 * crossing / len(pairs):.1f}%)")


def corpus_targets(bundle: Path, op: str) -> Optional[List[tuple]]:
    """Per-command rendered param targets: `(from, to)` for edge shapes, `(id,)` for node
    shapes, or None for the known rand()-targeted ops. A command without a recognizable
    target in any other op — or a mix of target shapes within one op — is an error."""
    if op in RAND_TARGETED:
        return None
    targets: List[tuple] = []
    with open(bundle / "commands" / f"{op}.jsonl") as fh:
        for seq, line in enumerate(fh):
            record = json.loads(line)
            params = dict(PARAMS.findall(record["cypher"].split(" MATCH", 1)[0].split(" MERGE", 1)[0]))
            if "from" in params and "to" in params:
                targets.append((int(params["from"]), int(params["to"])))
            elif "id" in params:
                targets.append((int(params["id"]),))
            else:
                sys.exit(f"{op} seq {seq}: no `id` or `from`/`to` param target — fail closed")
            if len(targets[0]) != len(targets[-1]):
                sys.exit(f"{op} seq {seq}: mixed target shapes within one op — fail closed")
    return targets


def corpus_report(bundle: Path, workers: int) -> None:
    ops = sorted(p.stem for p in (bundle / "commands").glob("*.jsonl"))
    for op in ops:
        targets = corpus_targets(bundle, op)
        if targets is None:
            print(f"{op:32} skipped (no param target — rand()-picked)")
            continue
        k = len(targets)
        chunk = split_size(k, workers, f"{op} corpus size")
        touch_seqs: Dict[int, List[int]] = {}
        for seq, target in enumerate(targets):
            for node_id in set(target):
                touch_seqs.setdefault(node_id, []).append(seq)
        repeat_touches = sum(len(seqs) - 1 for seqs in touch_seqs.values())
        cross_worker = sum(
            1
            for seqs in touch_seqs.values()
            for later, seq in enumerate(seqs[1:], 1)
            if any(seq // chunk != seqs[earlier] // chunk for earlier in range(later))
        )
        pair_note = ""
        if targets and len(targets[0]) == 2:
            dup_pairs = k - len(set(targets))
            pair_note = f"  directed (from,to) pairs: {'all unique' if dup_pairs == 0 else f'{dup_pairs} repeated'}"
        print(
            f"{op:32} K={k}  repeat-touches={repeat_touches:4}  "
            f"cross-worker (chunk={chunk}): {cross_worker:3}{pair_note}"
        )


def main() -> None:
    if len(sys.argv) < 3 or sys.argv[1] not in ("edges", "corpus"):
        sys.exit(__doc__)
    bundle = Path(sys.argv[2])
    workers = int(sys.argv[3]) if len(sys.argv) > 3 else 8
    if sys.argv[1] == "edges":
        edges_report(bundle, workers)
    else:
        corpus_report(bundle, workers)


if __name__ == "__main__":
    main()
