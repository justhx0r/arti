#!/bin/bash
set -xe

if [ -z "$RUST_LOG" ]; then
    echo "Setting RUST_LOG=info for your convenience."
    export RUST_LOG=info
fi

target="chutney/networks/basic"
cd "$(git rev-parse --show-toplevel)"
[ -d chutney ] || git clone https://gitlab.torproject.org/tpo/core/chutney
./chutney/chutney configure "$target"
./chutney/chutney start "$target"
CHUTNEY_START_TIME=180 ./chutney/chutney wait_for_bootstrap "$target"
./chutney/chutney verify "$target"

cargo run -p arti-bench --release -- -c chutney/net/nodes/arti.toml "$@"

./chutney/chutney stop "$target"
