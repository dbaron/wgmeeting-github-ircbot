use std::env;
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::process::Command;

fn main() {
    // Write the hash of our current revision to the file
    // ${OUT_DIR}/git-hash, so that it can be tetxually included in the
    // binary using include_str!() in src/main.rs.
    let output_dir = env::var("OUT_DIR").unwrap();
    let output_file_path = Path::new(&output_dir).join("git-hash");
    let mut output_file = File::create(&output_file_path).unwrap();

    let git_command = Command::new("git")
        .args(&["rev-parse", "HEAD"])
        .output()
        .unwrap();

    output_file
        .write_all(git_command.stdout.as_slice())
        .unwrap();
}
