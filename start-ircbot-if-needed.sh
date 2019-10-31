#!/bin/bash

if ! ps -C screen > /dev/null
then
	screen -d -m bash -l -c "(cd ~/wgmeeting-github-ircbot && git pull && RUST_BACKTRACE=1 RUST_LOG=wgmeeting_github_ircbot cargo run --release ./src/config.toml ./github_access_token_file) > ~/logs/ircbot.$(date +%F.%H%M%S).$$.log 2>&1"
fi
