//! Rustab's elevated helper: the whole root-privileged surface of the app,
//! in one GUI-free binary - see `docs/API_CONTRACT.md` §4.
//!
//! Reads one JSON request on stdin, performs it on `/etc/fstab`, prints one
//! JSON response on stdout, exits with `0`/`1`/`2`. No arguments, no
//! environment, no logging, no configuration: the only thing that can
//! influence what this process does is the request on its stdin, and the
//! only file it will act on is the hardcoded
//! [`rustab_privileged::target_identity::REAL_FSTAB_PATH`].
//!
//! It exists because the GUI binary is dynamically linked against GTK,
//! WebKitGTK and the system glibc, which is fine when `pkexec` re-executes
//! it on the same machine it was built for, but not under Flatpak: there
//! the GUI's libraries come from the Flatpak runtime, and the `pkexec` has
//! to run on the **host**, which may satisfy none of them. This binary's
//! entire dependency list is `rustab-privileged` (and so `fstab-core`,
//! `serde`, `serde_json`), which is what lets it be linked as a
//! self-contained static executable that runs on any host.

use rustab_privileged::protocol::{
    handle_privileged_request, protocol_error_body, EXIT_PROTOCOL_ERROR,
};
use rustab_privileged::target_identity::REAL_FSTAB_PATH;
use std::io;
use std::path::Path;

fn main() {
    // Nothing but argv[0] is accepted. Any extra argument means the caller
    // is not the Rustab build this binary belongs to, and a root process is
    // the wrong place to start guessing what was meant. The refusal is
    // reported in the protocol's own error shape on stdout - the caller
    // parses that - with the human-readable half on stderr.
    if std::env::args_os().nth(1).is_some() {
        println!(
            "{}",
            protocol_error_body("rustab-helper takes no arguments; nothing was done")
        );
        eprintln!("rustab-helper takes no arguments; it reads one JSON request on stdin");
        std::process::exit(EXIT_PROTOCOL_ERROR);
    }

    let mut stdin = io::stdin().lock();
    let mut stdout = io::stdout().lock();
    let code = handle_privileged_request(&mut stdin, &mut stdout, Path::new(REAL_FSTAB_PATH));
    std::process::exit(code);
}
