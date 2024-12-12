[![Cargo Build & Test](https://github.com/FalkorDB/benchmark/actions/workflows/ci.yml/badge.svg)](https://github.com/FalkorDB/benchmark/actions/workflows/ci.yml)
[![License](https://img.shields.io/github/license/falkordb/benchmark.svg)](https://github.com/falkordb/benchmark/blob/main/LICENSE)
[![Discord](https://img.shields.io/discord/1146782921294884966.svg?style=social&logo=discord)](https://discord.com/invite/99y2Ubh6tg)
[![Twitter](https://img.shields.io/twitter/follow/falkordb?style=social)](https://twitter.com/falkordb)

![falkor benchmarks logo](https://falkordb.github.io/benchmark/images/falkor_benchamrks.png)

[View the Benchmark Results](https://falkordb.github.io/benchmark/index.html)

# Key Benchmark Takeaways

Get mission-critical performance even under extreme workloads, with response times staying under 140ms at p99, while
competitors struggle with multi-second latencies. Reduce infrastructure costs and improve user experience with
FalkorDB's superior performance profile, requiring fewer resources to handle peak workloads.

| Percentile       | FalkorDB (ms) | Neo4j (ms) | Performance Difference |
|------------------|---------------|------------|------------------------|
| **p50 (median)** | 55.0          | 577.5      | 10.5x faster           |
| **p90**          | 108.0         | 4784.1     | 44.3x faster           |
| **p99**          | 136.2         | 46923.8    | 344.5x faster          |

## About the benchmarks

This benchmark provides comprehensive performance comparisons between FalkorDB and Neo4j graph databases. This benchmark
specifically focuses on aggregate expansion operations, a common workload in graph database applications. The results
indicate FalkorDB's particular strength in maintaining consistent performance under varying workload conditions,
especially crucial for production environments where predictable response times are essential.

## System Requirements

### Prerequisites

- Ubuntu
- Redis server
- build-essential, cmake, m4, automake
- libtool, autoconf, python3
- libomp-dev, libssl-dev
- pkg-config
- Rust toolchain
- SDKman
- unzip, zip

Installation Steps
==================

#### install redis server

```bash
sudo apt-get install lsb-release curl gpg
curl -fsSL https://packages.redis.io/gpg | sudo gpg --dearmor -o /usr/share/keyrings/redis-archive-keyring.gpg
sudo chmod 644 /usr/share/keyrings/redis-archive-keyring.gpg
echo "deb [signed-by=/usr/share/keyrings/redis-archive-keyring.gpg] https://packages.redis.io/deb $(lsb_release -cs) main" | sudo tee /etc/apt/sources.list.d/redis.list
sudo apt-get update
sudo apt-get install redis
```

- stop the redis server `sudo systemctl stop redis-server`
- disable the redis server `sudo systemctl disable redis-server`
- check the redis server status `sudo systemctl status redis-server`

#### install sdkman

- install unzip `sudo apt install unzip zip -y`
- `curl -s "https://get.sdkman.io" | bash`
- load sdkman in the current shell `source "$HOME/.sdkman/bin/sdkman-init.sh"`

#### build falkordb from source

- `git clone --recurse-submodules -j8 https://github.com/FalkorDB/FalkorDB.git`
- `sudo apt install build-essential cmake m4 automake libtool autoconf python3 libomp-dev libssl-dev`
- install rust `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
- from FalkorDB root dir run `make`

#### build the benchmark from source

from `~/`

- install pkg-config `sudo apt install pkg-config -y`
- `git clone git@github.com:FalkorDB/benchmark.git`
- `cd benchmark`
- `sdk env install`
- download and unpack neo4j `./scripts/download-neo4j.sh`
- build the benchmark `cargo build --release`
- enable autocomplete `source <(./target/release/benchmark generate-auto-complete bash)`
- copy the falkor shared lib to `cp ~/FalkorDB/bin/linux-x64-release/src/falkordb.so .`

#### run the benchmark

The benchmark is a cli tool that can be used to run the benchmarks

```bash
➜  cargo run  --bin benchmark -- --help                                                                  git:(prometheus|✚7…3
    
Usage: benchmark <COMMAND>

Commands:
  generate-auto-complete
  load                    load data into the database
  generate-queries        generate a set of queries and store them in a file to be used with the run command
  run                     run the queries generated by the GenerateQueries command against the chosen vendor
  help                    Print this message or the help of the given subcommand(s)

Options:
  -h, --help     Print help
  -V, --version  Print version
```

##### load the data

- `cargo run --release --bin benchmark -- load --vendor falkor -s small`
- `cargo run --release --bin benchmark -- load --vendor neo4j -s small`

##### create a set of queries to be used with the run command

- `cargo run --release --bin benchmark -- generate-queries  -s10000000 --dataset small --name=small`

##### run the benchmarks

- `cargo run --release --bin benchmark run --vendor falkor --name small -p40 --mps 2500`

##### run simulation to see that the benchmark itself can sustain spesific mps given a fix latency on that hardware

For example, simulate 40 clients that send at 5000 messages per seconds with latency of one millisecond per call.

- `cargo run --release --bin benchmark run --vendor falkor --name small -p40 --mps 5000 --simulate 1'

### Data

The data is based on https://www.kaggle.com/datasets/wolfram77/graphs-snap-soc-pokec
licensed: https://creativecommons.org/licenses/by/4.0/

## FAQ

### System Requirements

**Q: What are the minimum system requirements?**  
A: FalkorDB requires a Linux/Unix system with 4GB RAM minimum. For production environments, 16GB RAM is recommended.

### Installation & Setup

**Q: Can I run FalkorDB without Redis?**  
A: No, FalkorDB requires Redis 6.2 or higher as it operates as a Redis module.

### Development

**Q: Which query language does FalkorDB use?**  
A: FalkorDB uses the Cypher query language, similar to Neo4j, making migration straightforward.

### Data Management

**Q: Does FalkorDB support data persistence?**  
A: Yes, through Redis persistence mechanisms (RDB/AOF). Additional persistence options are in development.

### Integration

**Q: Does FalkorDB support common programming languages?**  
A: Yes, through FalkorDB has set of clients in all these programming langauges and more
see [official clients](https://docs.falkordb.com/clients.html)

### Production Use

**Q: Is FalkorDB production-ready?**  
A: Yes, FalkorDB is stable for production use, being a continuation of the battle-tested RedisGraph codebase.

### Troubleshooting

**Q: What should I do if I get "libgomp.so.1: cannot open shared object file"?**  
A: Install OpenMP:

- Ubuntu: `apt-get install libgomp1`
- RHEL/CentOS: `yum install libgomp`
- OSX: `brew install libomp`

### Migration

**Q: Can I migrate from Neo4j to FalkorDB?**  
A: Yes, FalkorDB supports the Cypher query language, making migration from Neo4j straightforward. Migration tools are in
development.

### Grafana and Prometheus

- Accessing grafana http://localhost:3000
- Accessing prometheus http://localhost:9090
- sum by (vendor, spawn_id)  (rate(operations_total{vendor="falkor"}[1m]))
  redis
- rate(redis_commands_processed_total{instance=~"redis-exporter:9121"}[1m])
- redis_connected_clients{instance=~"redis-exporter:9121"}
- topk(5, irate(redis_commands_total{instance=~"redis-exporter:9121"} [1m]))
- redis_blocked_clients
- redis_commands_total
- redis_commands_failed_calls_total
- redis_commands_latencies_usec_count
- redis_commands_rejected_calls_total
- redis_io_threaded_reads_processed
- redis_io_threaded_writes_processed
- redis_io_threads_active
- redis_memory_max_bytes
- redis_memory_used_bytes
- redis_memory_used_peak_bytes
- redis_memory_used_vm_total
- redis_process_id
  =======


