# The plan

This is an IRC bot to help solve a problem we have in [CSS Working
Group](https://wiki.csswg.org/) meetings, which is that we discuss a
topic for a while that has a github issue, and then fail to make a note
of that discussion in the github issue.  Since minute taking in meetings
happens in IRC, an IRC bot is useful here.

The idea is that the bot will be in the channel, will split the IRC up
based on "Topic:" and start/end of meeting, and then if somebody writes
"Github topic: &lt;github-url>" at some point within a topic
(changeable/cancellable also with acknowledgment), it will acknowledge
it and then when the topic concludes, make a github comment in that
issue at the end of the topic with the resolutions, and a &lt;details>
with the full IRC log, or something like that.  I'll probably also teach
it to understand "Topic:" itself being a github URL.

(Ideally it will also understand ScribeNick: and the other
scribe.perl conventions, but that's past minimum-viable-product, I
think.   Though ScribeNick should probably be doable quickly...)

My previous notes:
* [X] "Topic github: &lt;url>" or just "Topic: &lt;github-url>"
    * [X] acknowledge this
* [X] split on "Topic:" and "trackbot, end meeting"
    * [X] acknowledge again after making comment
* [X] answer help command asked explicitly
* [X] answer other requests asked explicitly
* [ ] answer PMs saying need to be in channel
* [ ] Alan Stearns suggests also removing the Agenda+ or Agenda+ F2F tags.


# Development notes

Put the github API key in ./src/config.json and then:

    RUST_BACKTRACE=1 RUST_LOG=wgmeeting_github_ircbot cargo run [--release] ./src/config.json
