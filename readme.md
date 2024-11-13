Install on Ubuntu
=================

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

##### init the databases

- `./target/release/benchmark init --vendor falkor -s small`
- `./target/release/benchmark init --vendor neo4j -s small`

##### run the benchmarks

- `./target/release/benchmark run --vendor falkor -ssmall -q100000`
- `./target/release/benchmark run --vendor neo4j -ssmall -q100000`

##### comparing the results

- `./target/release/benchmark compare falkor-metrics_small_q100000.json neo4j-metrics_small_q100000.json`

### Data

The data is based on https://www.kaggle.com/datasets/wolfram77/graphs-snap-soc-pokec
licensed: https://creativecommons.org/licenses/by/4.0/

