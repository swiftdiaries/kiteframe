#!/usr/bin/env bash
set -euo pipefail

cargo fuzz run strict_yaml fuzz/seeds/strict_yaml/byte-limit -- -runs=1 -seed=1
cargo fuzz run strict_yaml fuzz/seeds/strict_yaml/nesting-limit -- -runs=1 -seed=1
cargo fuzz run strict_yaml fuzz/seeds/strict_yaml/collection-limit -- -runs=1 -seed=1
cargo fuzz run strict_yaml fuzz/seeds/strict_yaml/alias-limit -- -runs=1 -seed=1
