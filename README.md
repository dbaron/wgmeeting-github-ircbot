# The plan

This is an IRC bot to help solve a problem we have in [CSS Working
Group](https://wiki.csswg.org/) meetings, which is that we discuss a
topic for a while that has a GitHub issue, and then fail to make a note
of that discussion in the GitHub issue.  Since minute taking in meetings
happens in IRC, an IRC bot is useful here.

The idea is that the bot will be in the channel, will split the IRC up
based on "Topic:" and start/end of meeting, and then if somebody writes
"Github: &lt;github-url>" at some point within a topic
(changeable/cancellable also with acknowledgment), it will acknowledge
it and then when the topic concludes, make a comment in that GitHub
issue at the end of the topic with the resolutions, and a &lt;details>
with the full IRC log, or something like that.  (Understanding "Topic:"
itself being a github URL turned out badly because of multiple people
entering the same topic leading to multiple short or empty comments.)

(Ideally it will also understand ScribeNick: and the other
scribe.perl conventions, but that's past minimum-viable-product, I
think.   Though ScribeNick should probably be doable quickly...)

# How to use

Begin a topic on IRC:

```
Topic: [name of topic]
github: [URL of a GitHub issue]
```

The bot responds:

```
* github-bot OK, I'll post this discussion to [URL of the GitHub issue]
```

Discuss the topic on IRC.

Add the resolutions:

```
RESOLVED: frob the snozwuzzle breadth-first
```

Either begin a new topic:

```
Topic: Semantics of the gribble
```

or explictly end the topic:

```
github-bot, end topic
```

At this point, the github-bot responds:

```
* github-bot Successfully commented on [URL of the GitHub issue]
```

The comments that github-bot adds are everything since the last Topic was begun, even if that was before the `github: [URL]` was entered.

# Development notes

If you don't have Rust installed, start with [rustup](https://rustup.rs/).

If you want to use the bot to generate real GitHub comments, you'll need
to [generate a GitHub personal access
token](https://github.com/settings/tokens) while logged in to the GitHub
account that you want to make the comments, and put the personal access
token in a file (say, `./github_access_token_file`).  Then you can
compile and run the bot with a single [`cargo`](http://doc.crates.io/)
command, such as one of:

    RUST_BACKTRACE=1 RUST_LOG=wgmeeting_github_ircbot cargo run ./src/config-dev.toml ./github_access_token_file
    RUST_BACKTRACE=1 RUST_LOG=wgmeeting_github_ircbot cargo run --release ./src/config.toml ./github_access_token_file

Or you could just run automated tests with a different single `cargo`
command (which doesn't require an access token):

    RUST_BACKTRACE=1 RUST_LOG=wgmeeting_github_ircbot,test_chats cargo test

or for more verbosity:

    RUST_BACKTRACE=1 RUST_LOG=wgmeeting_github_ircbot,test_chats,tokio_core,tokio_reactor cargo test

# Do you want this bot for your working group?

If you want this bot for your working group that minutes its
teleconferences on `irc.w3.org`, I'm happy to take pull requests to this
repository.  You need to add a new `channels` item in `src/config.toml`.
The channel name in the header gives the IRC channel, the `group` gives
the name of the working group used in comments on github issues, and the
`github_repos_allowed` line lists github repositories that the bot is
allowed to comment in.

# Acknowledgments

Thanks to Xidorn Quan and Alan Stearns for feature suggestions, and to
Manish Goregaokar, Simon Sapin, Jack Moffitt, and Till Schneidereit for
answering my questions about Rust while I was trying to learn Rust while
writing this.
