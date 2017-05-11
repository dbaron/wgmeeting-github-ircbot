// see 'rustc -W help'
#![warn(missing_docs, unused_extern_crates, unused_results)]

//! Test all of the tests in chats/, which are .txt files formatted with IRC
//! input beginning with <, expected IRC output beginning with >, and expected
//! github output beginning with !.

#[macro_use]
extern crate log;
extern crate env_logger;
extern crate wgmeeting_github_ircbot;
extern crate irc;
extern crate diff;

use irc::client::conn::MockConnection;
use irc::client::prelude::*;
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::iter::FromIterator;
use std::path::Path;
use std::str;
use wgmeeting_github_ircbot::*;

#[test]
fn test_chats() {
    env_logger::init().unwrap();

    let chats_dir = Path::new(file!()).parent().unwrap().join("chats");
    info!("Going through {:?}", chats_dir);
    let mut fail_count = 0;
    for direntry in chats_dir.read_dir().unwrap() {
        if let Ok(direntry) = direntry {
            if !test_one_chat(direntry.path().as_path()) {
                fail_count += 1;
            }
        }
    }
    assert!(fail_count == 0,
            format!("{} chat test failure(s), see above", fail_count));
}

#[allow(unused_results)]
fn test_one_chat(path: &Path) -> bool {
    info!("Testing {:?}", path);

    let file_data = {
        let mut file_data: Vec<u8> = Vec::new();
        File::open(path)
            .unwrap()
            .read_to_end(&mut file_data)
            .unwrap();
        file_data
    };

    let irc_config: Config = Config {
        owners: Some(vec![format!("dbaron")]),
        nickname: Some(format!("test-github-bot")),
        alt_nicks: Some(vec![format!("test-github-bot-"), format!("test-github-bot--")]),
        username: Some(format!("dbaron-gh-bot")),
        realname: Some(format!("Bot to add meeting minutes to github issues.")),
        server: Some(format!("irc.w3.org")),
        port: Some(6667),
        use_ssl: Some(false),
        encoding: Some(format!("UTF-8")),
        channels: Some(vec![format!("#meetingbottest")]),
        user_info: Some(format!("Bot to add meeting minutes to github issues.")),
        // FIXME: why doesn't this work as documented?
        // source: Some(format!("https://github.
        // com/dbaron/wgmeeting-github-ircbot")),
        options: Some(HashMap::from_iter(vec![(format!("source"),
                                               format!("https://github.\
                                                        com/dbaron/wgmeeting-github-ircbot")),
                                              (format!("github_repos_allowed"),
                                               format!("dbaron/wgmeeting-github-ircbot \
                                                        dbaron/nonexistentrepo"))])),
        ..Default::default()
    };

    let mut server_read_data: Vec<u8> = Vec::new();
    let mut expected_result_data: Vec<u8> =
        "CAP END\r\nNICK :test-github-bot\r\nUSER dbaron-gh-bot 0 * :Bot \
         to add meeting minutes to github issues.\r\n"
                .bytes()
                .collect();

    // FIXME: This doesn't test that the responses are sent at the right
    // times.  Doing that requires writing a new version of
    // MockConnection that can have dynamic additions made to the read
    // buffer.  Then we could build a result that represents the entire
    // chat rather than just the pieces of it.  This also requires
    // integrating this loop and the loop below.
    for line in file_data.split(|byte| *byte == '\n' as u8) {
        match line.first().map(|b| *b as char) {
            // just skip blank lines (and the empty string after the last
            // line, given the use of split)
            None => (),
            Some('<') => {
                info!("adding line to read buffer: {}",
                      str::from_utf8(line).unwrap());
                server_read_data.extend_from_slice(&line[1 ..]);
                server_read_data.append(&mut "\r\n".bytes().collect());
                // result_data.extend_from_slice(line);
                // result_data.append(&mut "\r\n".bytes().collect());
                // expected_result_data.extend_from_slice(line);
                // expected_result_data.append(&mut "\r\n".bytes().collect());
            }
            Some('>') => {
                info!("adding line to expected results: {}",
                      str::from_utf8(line).unwrap());
                // expected_result_data.extend_from_slice(line);
                // expected_result_data.append(&mut "\r\n".bytes().collect());

                // FIXME: Clean up this total hack for \u{1} !
                let mut line = str::from_utf8(line).unwrap().replace("\\u{1}", "\u{1}");
                expected_result_data.append(&mut line[1 ..].bytes().collect());
                expected_result_data.append(&mut "\r\n".bytes().collect());
            }
            Some('!') => {
                // FIXME: Need to get these in the actual data, too!
                info!("adding line to expected results: {}",
                      str::from_utf8(line).unwrap());
                // for now, we send the github comments over IRC when
                // testing, but we don't encode that into the chat
                // format
                expected_result_data.append(&mut "PRIVMSG github-comments :".bytes().collect());
                expected_result_data.extend_from_slice(&line[1 ..]);
                expected_result_data.append(&mut "\r\n".bytes().collect());
            }
            _ => {
                panic!("Unexpected line in test file {:?}:\n{}",
                       path,
                       str::from_utf8(line).unwrap_or("[non-UTF-8 line]"));
            }
        }
    }

    let server = IrcServer::from_connection(irc_config,
                                            MockConnection::from_byte_vec(server_read_data));
    let options = server
        .config()
        .options
        .as_ref()
        .expect("No options property within configuration?");
    let mut irc_state = IRCState::new(GithubType::MockGithubConnection);
    let conn = server.conn();

    server.identify().unwrap();

    // FIXME: Integrate these two loops!
    // When they're integrated, there's no need to repeat this inner
    // loop after the outer loop because the outer loop should always
    // end with a blank line.
    for message in server.iter() {
        info!("processing message {:?}", message);
        main_loop_iteration(server.clone(),
                            &mut irc_state,
                            options,
                            message.as_ref().unwrap());
    }

    let actual_str = &*conn.written("UTF-8").unwrap();
    let expected_str = str::from_utf8(expected_result_data.as_slice()).unwrap();
    let pass = actual_str == expected_str;
    println!("\n{:?} {}", path, if pass { "PASS" } else { "FAIL" });
    if !pass {
        for d in diff::lines(expected_str, actual_str) {
            match d {
                diff::Result::Left(actual) => println!("-{}", actual),
                diff::Result::Both(actual, _) => println!(" {}", actual),
                diff::Result::Right(expected) => println!("+{}", expected),
            }
        }
    }

    pass
}
