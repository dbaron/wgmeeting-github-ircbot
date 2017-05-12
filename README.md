# The plan

This is an IRC bot to help solve a problem we have in [CSS Working
Group](https://wiki.csswg.org/) meetings, which is that we discuss a
topic for a while that has a GitHub issue, and then fail to make a note
of that discussion in the GitHub issue.  Since minute taking in meetings
happens in IRC, an IRC bot is useful here.

The idea is that the bot will be in the channel, will split the IRC up
based on "Topic:" and start/end of meeting, and then if somebody writes
"Github topic: &lt;github-url>" at some point within a topic
(changeable/cancellable also with acknowledgment), it will acknowledge
it and then when the topic concludes, make a comment in that GitHub
issue at the end of the topic with the resolutions, and a &lt;details>
with the full IRC log, or something like that.  (Understanding "Topic:"
itself being a github URL turned out badly because of multiple people
entering the same topic leading to multiple short or empty comments.)

(Ideally it will also understand ScribeNick: and the other
scribe.perl conventions, but that's past minimum-viable-product, I
think.   Though ScribeNick should probably be doable quickly...)

# Development notes

If you don't have Rust installed, start with [rustup](https://rustup.rs/).

If you want to use the bot to generate real GitHub comments, you'll need
to [generate a GitHub personal access
token](https://github.com/settings/tokens) while logged in to the GitHub
account that you want to make the comments, and put the personal access
token in the `github_access_token` field in 
in `./src/config.json`.  Then you can run the bot with one of:

    RUST_BACKTRACE=1 RUST_LOG=wgmeeting_github_ircbot cargo run ./src/config-dev.json
    RUST_BACKTRACE=1 RUST_LOG=wgmeeting_github_ircbot cargo run --release ./src/config.json

Or you could just run automated tests, which doesn't require an access
token:

    RUST_BACKTRACE=1 RUST_LOG=wgmeeting_github_ircbot,test_chats cargo test

# Acknowledgments

Thanks to Xidorn Quan and Alan Stearns for feature suggestions, and to
Manish Goregaokar, Simon Sapin, Jack Moffitt, and Till Schneidereit for
answering my questions about Rust while I was trying to learn Rust while
writing this.
