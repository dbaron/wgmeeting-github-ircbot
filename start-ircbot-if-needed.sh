#!/bin/bash

if ! ps -C screen -o user | grep "^ircbot$" > /dev/null
then
	screen -d -m bash -l -c "(cd ~/wgmeeting-github-ircbot && git pull && RUST_BACKTRACE=1 RUST_LOG=wgmeeting_github_ircbot cargo run -j1 --release ./src/config.toml ./github_access_token_file) > ~/logs/ircbot.$(date +%F.%H%M%S).$$.log 2>&1"
fi
