**FalkorDB telemetry streams — offline analysis guide**  
FalkorDB publishes **per-query telemetry** into **Redis Streams**. Each graph gets its own stream. This document describes:  
1. Stream **key names**  
2. **Message structure** (fields and meaning)  
3. How to **collect and analyze** the data with Redis commands for offline use  
  
---  
**1. Stream key names**  
**Pattern**  
```  
telemetry{<graph_name>}  
   
```  
- Prefix: literal `telemetry`  
- Hash-tag: `{<graph_name>}` — the graph key as known to FalkorDB / Redis  
Examples:  
```  
telemetry{my_graph}  
 telemetry{customers}  
   
```  
**Discover all telemetry streams**  
```  
redis-cli -h 127.0.0.1 -p 6379 --scan --pattern 'telemetry*'  
   
```  
Or:  
```  
redis-cli -h 127.0.0.1 -p 6379 KEYS 'telemetry*'  
   
```  
(`SCAN` is preferred on production; `KEYS` is fine on local/dev.)  
**List graphs (to predict stream names)**  
```  
redis-cli -h 127.0.0.1 -p 6379 GRAPH.LIST  
   
```  
For each graph name `G`, expect stream key `telemetry{G}` once queries have been executed against that graph (the stream is created when FalkorDB starts emitting telemetry for it).  
**Type check**  
```  
redis-cli -h 127.0.0.1 -p 6379 TYPE 'telemetry{my_graph}'  
 # => stream  
   
```  
Always **quote** stream keys in the shell so the braces are not expanded by zsh/bash:  
```  
# unquoted  → zsh may error: no matches found: telemetry{my_graph}  
 # quoted    → works  
 redis-cli TYPE 'telemetry{my_graph}'  
   
```  
  
---  
**2. Message structure**  
Each stream entry is a standard Redis Stream record:  
```  
id:   <millisecondsTime>-<sequence>     e.g. 1762527691287-0  
 fields: flat string key/value pairs  
   
```  
**Field catalog (all values are strings in Redis)**  
  
|  |  |  |  |  
| - | - | - | - |  
| **Field** |  **Type (logical)** |  **Unit / format** |  **Description** |  
| `Received at` |  number as string |  Unix time (seconds, fractional) |  When the server accepted/received the query |  
| `Query` |  string |  Cypher text |  Query text (may include `$params`; whitespace as sent) |  
| `Query parameters` |  string |  free text / serialized params |  Parameter payload associated with the query |  
| `Total duration` |  number as string |  **milliseconds** |  End-to-end time on the server for this query |  
| `Wait duration` |  number as string |  **milliseconds** |  Queue / wait time before execution |  
| `Execution duration` |  number as string |  **milliseconds** |  Time spent executing the query plan |  
| `Report duration` |  number as string |  **milliseconds** |  Time spent producing/reporting the result |  
| `Utilized cache` |  `0` or `1` |  flag |  Whether a cached plan/execution path was used |  
| `Write` |  `0` or `1` |  flag |  Whether the query was a write |  
| `Timeout` |  `0` or `1` |  flag |  Whether the query hit a timeout |  
  
   
Relationship (approximately):  
```  
Total duration ≈ Wait duration + Execution duration + Report duration  
   
```  
(Minor residual is possible due to measurement boundaries.)  
**Example entry (abbreviated)**  
```  
ID: 1762527691287-0  
   Received at:          1762527691.052084  
   Query:                MATCH (e:__Entity__ {id: $eid}) RETURN e.name AS name, e.description AS desc  
   Query parameters:     ...  
   Total duration:       235.566792  
   Wait duration:        0.047000  
   Execution duration:   235.432000  
   Report duration:      0.087792  
   Utilized cache:       1  
   Write:                0  
   Timeout:              0  
   
```  
**Units**  
Treat duration fields in the Redis stream as **milliseconds** when analyzing raw data.  
```  
seconds      = ms / 1000  
 microseconds = ms * 1000  
   
```  
  
---  
**3. Collecting telemetry offline with Redis commands**  
Replace `127.0.0.1`, `6379`, and `my_graph` with your host, port, and graph name.  
**3.1 Stream metadata**  
```  
redis-cli -h 127.0.0.1 -p 6379 XINFO STREAM 'telemetry{my_graph}'  
   
```  
Useful fields in the reply: `length`, `entries-added`, `first-entry`, `last-entry`, radix-tree stats.  
```  
redis-cli -h 127.0.0.1 -p 6379 XLEN 'telemetry{my_graph}'  
   
```  
**3.2 Read a window of history (**`XRANGE` **/** `XREVRANGE`**)**  
Oldest first, first 100 entries:  
```  
redis-cli -h 127.0.0.1 -p 6379 XRANGE 'telemetry{my_graph}' - + COUNT 100  
   
```  
Newest first:  
```  
redis-cli -h 127.0.0.1 -p 6379 XREVRANGE 'telemetry{my_graph}' + - COUNT 100  
   
```  
By ID range (inclusive):  
```  
redis-cli -h 127.0.0.1 -p 6379 XRANGE 'telemetry{my_graph}' 1762527690000-0 1762527700000-0  
   
```  
**3.3 Live tail (**`XREAD`**)**  
Follow new entries (block up to 5s each call):  
```  
# Start from "new messages only"  
 LAST='$'  
 while true; do  
   redis-cli -h 127.0.0.1 -p 6379 XREAD BLOCK 5000 COUNT 100 STREAMS 'telemetry{my_graph}' "$LAST"  
   # Parse last ID from output and set LAST=... for the next iteration  
 done  
   
```  
Or one-shot block:  
```  
redis-cli -h 127.0.0.1 -p 6379 XREAD BLOCK 5000 COUNT 10 STREAMS 'telemetry{my_graph}' '$'  
   
```  
**3.4 Dump entire stream to a file**  
**redis-cli text dump:**  
```  
redis-cli -h 127.0.0.1 -p 6379 --raw XRANGE 'telemetry{my_graph}' - + > telemetry_my_graph_xrange.txt  
   
```  
**JSON lines via Python (recommended):**  
```  
python3 - <<'PY'  
 import json, redis, sys  
   
 host, port, key = "127.0.0.1", 6379, "telemetry{my_graph}"  
 r = redis.Redis(host=host, port=port, decode_responses=True)  
   
 start, batch = "-", 1000  
 out_path = "telemetry_my_graph.jsonl"  
 total = 0  
   
 with open(out_path, "w") as out:  
     while True:  
         entries = r.xrange(key, min=start, max="+", count=batch)  
         if not entries:  
             break  
         for eid, fields in entries:  
             rec = {"id": eid, **fields}  
             out.write(json.dumps(rec, ensure_ascii=False) + "\n")  
             total += 1  
         # XRANGE is inclusive; advance past last id  
         ms, seq = entries[-1][0].split("-", 1)  
         start = f"{ms}-{int(seq) + 1}"  
         if len(entries) < batch:  
             break  
   
 print(f"wrote {total} records to {out_path}", file=sys.stderr)  
 PY  
   
```  
**3.5 Export all telemetry streams on an instance**  
```  
python3 - <<'PY'  
 import json, redis  
   
 r = redis.Redis(host="127.0.0.1", port=6379, decode_responses=True)  
   
 for key in sorted(r.keys("telemetry*")):  
     path = key.replace("{", "_").replace("}", "") + ".jsonl"  
     n = 0  
     with open(path, "w") as f:  
         start = "-"  
         while True:  
             entries = r.xrange(key, min=start, max="+", count=1000)  
             if not entries:  
                 break  
             for eid, fields in entries:  
                 f.write(json.dumps({"stream": key, "id": eid, **fields}) + "\n")  
                 n += 1  
             ms, seq = entries[-1][0].split("-", 1)  
             start = f"{ms}-{int(seq) + 1}"  
             if len(entries) < 1000:  
                 break  
     print(key, "->", path, f"({n} entries)")  
 PY  
   
```  
**3.6 Simple offline aggregates**  
Average wait / exec / report (ms) from JSONL:  
```  
python3 - <<'PY'  
 import json  
 from collections import Counter  
   
 sum_w = sum_e = sum_r = n = 0  
 by_write = Counter()  
   
 with open("telemetry_my_graph.jsonl") as f:  
     for line in f:  
         o = json.loads(line)  
         n += 1  
         sum_w += float(o.get("Wait duration") or 0)  
         sum_e += float(o.get("Execution duration") or 0)  
         sum_r += float(o.get("Report duration") or 0)  
         by_write[o.get("Write", "?")] += 1  
   
 print("count", n)  
 print("avg wait_ms", sum_w / n if n else 0)  
 print("avg exec_ms", sum_e / n if n else 0)  
 print("avg report_ms", sum_r / n if n else 0)  
 print("write flag counts", dict(by_write))  
 PY  
   
```  
Filter timeouts only:  
```  
python3 - <<'PY'  
 import json  
   
 with open("telemetry_my_graph.jsonl") as f:  
     for line in f:  
         o = json.loads(line)  
         if o.get("Timeout") in ("1", 1, "true", True):  
             print(o["id"], o.get("Total duration"), (o.get("Query") or "")[:100])  
 PY  
   
```  
Top slowest queries by total duration:  
```  
python3 - <<'PY'  
 import json  
   
 rows = []  
 with open("telemetry_my_graph.jsonl") as f:  
     for line in f:  
         o = json.loads(line)  
         rows.append((float(o.get("Total duration") or 0), o.get("id"), o.get("Query") or ""))  
   
 for total, eid, q in sorted(rows, reverse=True)[:20]:  
     print(f"{total:10.3f} ms  {eid}  {q[:100]}")  
 PY  
   
```  
**3.7 Optional: consumer groups (multi-reader pipelines)**  
Telemetry streams typically have **no** consumer groups by default (`XINFO GROUPS` is empty). You can create one for durable workers:  
```  
redis-cli -h 127.0.0.1 -p 6379 \  
   XGROUP CREATE 'telemetry{my_graph}' offline_analytics 0 MKSTREAM  
   
 redis-cli -h 127.0.0.1 -p 6379 \  
   XREADGROUP GROUP offline_analytics worker1 COUNT 100 STREAMS 'telemetry{my_graph}' '>'  
   
 # After processing:  
 # redis-cli XACK 'telemetry{my_graph}' offline_analytics <id> ...  
   
```  
Use consumer groups when multiple collectors must share the stream without double-processing. Use `XRANGE` dumps when you want a full snapshot for batch analysis.  
**3.8 Trim / retention (ops caution)**  
Streams can grow. `entries-added` (lifetime) may exceed current `length` after trims or evictions.  
```  
redis-cli -h 127.0.0.1 -p 6379 XLEN 'telemetry{my_graph}'  
   
```  
Do **not** `XTRIM` / `DEL` production telemetry keys unless you intend to drop history.  
  
---  
**4. Data path (conceptual)**  
```  
Client issues Cypher (GRAPH.QUERY / GRAPH.RO_QUERY / …)  
         │  
         ▼  
 FalkorDB executes query  
         │  
         ▼  
 Redis Stream  telemetry{<graph_name>}  
   one entry per instrumented query  
   fields: Query, durations, flags, …  
         │  
         ▼  
 Your offline consumer  
   XRANGE / XREAD / XREADGROUP  →  files, warehouse, dashboards  
   
```  
  
---  
**5. Quick reference cheat sheet**  
```  
# Discover  
 redis-cli --scan --pattern 'telemetry*'  
 redis-cli GRAPH.LIST  
   
 # Inspect  
 redis-cli TYPE 'telemetry{my_graph}'  
 redis-cli XLEN 'telemetry{my_graph}'  
 redis-cli XINFO STREAM 'telemetry{my_graph}'  
 redis-cli XREVRANGE 'telemetry{my_graph}' + - COUNT 5  
   
 # Dump  
 redis-cli --raw XRANGE 'telemetry{my_graph}' - + > dump.txt  
   
 # Live follow  
 redis-cli XREAD BLOCK 5000 COUNT 50 STREAMS 'telemetry{my_graph}' '$'  
   
```  
**Message fields**  
```  
Received at  
 Query  
 Query parameters  
 Total duration          # ms (string)  
 Wait duration           # ms (string)  — queue/wait  
 Execution duration      # ms (string)  — execute  
 Report duration         # ms (string)  — result report  
 Utilized cache          # "0" | "1"  
 Write                   # "0" | "1"  
 Timeout                 # "0" | "1"  
   
```  
  
---  
**6. Caveats**  
1. **Stream key is per graph name**, not global. Wrong graph ⇒ empty reply or `ERR no such key`.  
2. **Hash tags** `{graph}` matter for Redis Cluster slot placement; always quote keys: `'telemetry{my_graph}'`.  
3. **Duration fields are milliseconds** in the stream (string-encoded floating point).  
4. **`Query` / `Query parameters`** retain parameter values and original whitespace; exact string joins across tools may need normalization.  
5. Streams may be **capped or trimmed** by server policy; `entries-added` can exceed current `XLEN`.  
6. Telemetry emission is a **FalkorDB server feature**; if no queries ran (or telemetry is disabled in a given build/config), no stream key appears.  
