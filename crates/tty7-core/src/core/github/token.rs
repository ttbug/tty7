//! Where the GitHub token comes from.
//!
//! tty7 has no OAuth app and stores no credential of its own: it borrows the
//! one the user already gave the GitHub CLI. In order:
//!
//! 1. `GH_TOKEN`, then `GITHUB_TOKEN` — the variables `gh` itself honours, so
//!    a CI-style setup behaves the same here as in the shell;
//! 2. `gh auth token --hostname github.com`.
//!
//! No token is not an error. Public repositories read fine without one, at
//! GitHub's unauthenticated rate (60 requests an hour).
//!
//! `gh` is looked for on `PATH` *and* at the Homebrew and `/usr/local`
//! locations, because an app launched from Finder or the Dock inherits
//! launchd's minimal `PATH` rather than the login shell's, and `gh` is almost
//! never on it there.
//!
//! The token never reaches a log line or the screen: [`Token`]'s `Debug`
//! prints a placeholder, and nothing else in tty7 formats it.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// A GitHub token. Opaque on purpose — see the module docs.
#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

impl Token {
    /// A token from wherever it was found, or `None` for an empty value
    /// (`GH_TOKEN=` set but blank is "no token", as it is for `gh`).
    pub fn new(raw: &str) -> Option<Token> {
        let raw = raw.trim();
        (!raw.is_empty() && !raw.contains(char::is_whitespace)).then(|| Token(raw.to_string()))
    }

    /// For the `Authorization` header, and for nothing else.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Token(<redacted>)")
    }
}

/// Where a token was found, for the panel to say so ("signed in via gh").
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TokenSource {
    Env(&'static str),
    GhCli,
}

/// The environment variables consulted, in order.
pub const TOKEN_VARS: [&str; 2] = ["GH_TOKEN", "GITHUB_TOKEN"];

/// Fixed places `gh` lands when installed outside the login `PATH`.
pub const GH_FALLBACKS: [&str; 3] = [
    "/opt/homebrew/bin/gh",
    "/opt/homebrew/opt/gh/bin/gh",
    "/usr/local/bin/gh",
];

/// The side effects token resolution needs, injected so tests never read the
/// real environment or run the real `gh`.
pub struct TokenEnv<'a> {
    pub var: &'a dyn Fn(&str) -> Option<String>,
    pub is_file: &'a dyn Fn(&Path) -> bool,
    /// Run `<gh> auth token …` and hand back its stdout on success.
    pub gh_token: &'a dyn Fn(&Path) -> Option<String>,
}

/// Resolve a token, or `None` to go unauthenticated.
pub fn resolve_token(env: &TokenEnv<'_>) -> Option<(Token, TokenSource)> {
    for var in TOKEN_VARS {
        if let Some(token) = (env.var)(var).as_deref().and_then(Token::new) {
            return Some((token, TokenSource::Env(var)));
        }
    }
    // Only the first `gh` found is asked. A second install answering for a
    // different account than the one on PATH would be a surprise, not a
    // fallback.
    let gh = gh_candidates((env.var)("PATH").as_deref())
        .into_iter()
        .find(|p| (env.is_file)(p))?;
    let out = (env.gh_token)(&gh)?;
    // `gh auth token` prints the token and a newline; anything multi-line is
    // not a token.
    let line = out.lines().next()?;
    Token::new(line).map(|t| (t, TokenSource::GhCli))
}

/// Every place `gh` might be, `PATH` first, without duplicates.
pub fn gh_candidates(path_var: Option<&str>) -> Vec<PathBuf> {
    let exe = if cfg!(windows) { "gh.exe" } else { "gh" };
    let mut out: Vec<PathBuf> = Vec::new();
    if let Some(path) = path_var {
        for dir in std::env::split_paths(path) {
            if dir.as_os_str().is_empty() {
                continue;
            }
            out.push(dir.join(exe));
        }
    }
    if !cfg!(windows) {
        out.extend(GH_FALLBACKS.iter().map(PathBuf::from));
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|p| seen.insert(p.clone()));
    out
}

/// How long `gh auth token` may take. It reads a keyring, which is local and
/// fast — but a locked keyring can sit on a prompt, and the panel must not
/// wait on that forever.
const GH_TIMEOUT: Duration = Duration::from_secs(5);

/// [`resolve_token`] against the real process environment. Blocking: call it
/// off the UI thread.
pub fn resolve_token_from_system() -> Option<(Token, TokenSource)> {
    resolve_token(&TokenEnv {
        var: &|name| std::env::var(name).ok(),
        is_file: &|p| p.is_file(),
        gh_token: &run_gh_auth_token,
    })
}

fn run_gh_auth_token(gh: &Path) -> Option<String> {
    use crate::core::proc::{hide_console, output_within};
    let mut cmd = std::process::Command::new(gh);
    cmd.args(["auth", "token", "--hostname", "github.com"])
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1");
    // A GUI app spawning a console program on Windows flashes a console
    // window unless told not to.
    let out = output_within(hide_console(&mut cmd), GH_TIMEOUT).ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    struct Fake {
        vars: HashMap<&'static str, &'static str>,
        files: Vec<PathBuf>,
        gh_out: Option<&'static str>,
        asked: RefCell<Vec<PathBuf>>,
    }

    impl Fake {
        fn new() -> Fake {
            Fake {
                vars: HashMap::new(),
                files: Vec::new(),
                gh_out: None,
                asked: RefCell::new(Vec::new()),
            }
        }

        fn resolve(&self) -> Option<(Token, TokenSource)> {
            resolve_token(&TokenEnv {
                var: &|name| self.vars.get(name).map(|v| v.to_string()),
                is_file: &|p| self.files.iter().any(|f| f == p),
                gh_token: &|p| {
                    self.asked.borrow_mut().push(p.to_path_buf());
                    self.gh_out.map(str::to_string)
                },
            })
        }
    }

    #[test]
    fn gh_token_outranks_github_token_and_both_outrank_the_cli() {
        let mut f = Fake::new();
        f.vars.insert("GH_TOKEN", "from-gh-token");
        f.vars.insert("GITHUB_TOKEN", "from-github-token");
        f.files.push(PathBuf::from("/usr/local/bin/gh"));
        f.gh_out = Some("from-cli\n");
        let (token, source) = f.resolve().unwrap();
        assert_eq!(token.expose(), "from-gh-token");
        assert_eq!(source, TokenSource::Env("GH_TOKEN"));
        assert!(
            f.asked.borrow().is_empty(),
            "gh is not run when env answers"
        );

        f.vars.remove("GH_TOKEN");
        let (token, source) = f.resolve().unwrap();
        assert_eq!(token.expose(), "from-github-token");
        assert_eq!(source, TokenSource::Env("GITHUB_TOKEN"));
    }

    #[test]
    fn a_blank_variable_is_no_token_and_falls_through() {
        let mut f = Fake::new();
        f.vars.insert("GH_TOKEN", "   ");
        // A `gh` on PATH, spelled the way this platform spells it: the
        // Homebrew fallbacks don't exist on Windows, so the test must not
        // lean on them.
        let path = if cfg!(windows) {
            r"C:\fake\bin"
        } else {
            "/fake/bin"
        };
        f.vars.insert("PATH", path);
        let gh = gh_candidates(Some(path)).remove(0);
        f.files.push(gh.clone());
        f.gh_out = Some("gho_cli\n");
        let (token, source) = f.resolve().unwrap();
        assert_eq!(token.expose(), "gho_cli");
        assert_eq!(source, TokenSource::GhCli);
        assert_eq!(*f.asked.borrow(), vec![gh]);
    }

    #[cfg(unix)]
    #[test]
    fn gh_on_path_is_asked_before_the_homebrew_fallbacks() {
        let mut f = Fake::new();
        f.vars.insert("PATH", "/usr/bin:/home/me/bin");
        f.files.push(PathBuf::from("/home/me/bin/gh"));
        f.files.push(PathBuf::from("/opt/homebrew/bin/gh"));
        f.gh_out = Some("gho_x\n");
        f.resolve().unwrap();
        assert_eq!(*f.asked.borrow(), vec![PathBuf::from("/home/me/bin/gh")]);
    }

    #[cfg(unix)]
    #[test]
    fn a_finder_launch_still_finds_homebrew_gh() {
        let mut f = Fake::new();
        // launchd's PATH: none of it holds gh.
        f.vars.insert("PATH", "/usr/bin:/bin:/usr/sbin:/sbin");
        f.files.push(PathBuf::from("/opt/homebrew/opt/gh/bin/gh"));
        f.gh_out = Some("gho_brew\n");
        let (token, _) = f.resolve().unwrap();
        assert_eq!(token.expose(), "gho_brew");
        assert_eq!(
            *f.asked.borrow(),
            vec![PathBuf::from("/opt/homebrew/opt/gh/bin/gh")]
        );
    }

    #[test]
    fn no_env_and_no_gh_is_unauthenticated_not_an_error() {
        let f = Fake::new();
        assert!(f.resolve().is_none());
        assert!(f.asked.borrow().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_signed_out_gh_is_unauthenticated() {
        let mut f = Fake::new();
        f.files.push(PathBuf::from("/usr/local/bin/gh"));
        f.gh_out = None;
        assert!(f.resolve().is_none());
        f.gh_out = Some("\n");
        assert!(f.resolve().is_none());
    }

    #[test]
    fn candidates_skip_empty_path_entries_and_duplicates() {
        let c = gh_candidates(Some(""));
        let exe = if cfg!(windows) { "gh.exe" } else { "gh" };
        assert!(c.iter().all(|p| p.file_name().unwrap() == exe));
        let unique: std::collections::HashSet<_> = c.iter().collect();
        assert_eq!(unique.len(), c.len());
    }

    #[test]
    fn a_token_never_prints_itself() {
        let token = Token::new("ghp_secretsecret").unwrap();
        let shown = format!("{token:?} {:?}", Some((token.clone(), TokenSource::GhCli)));
        assert!(!shown.contains("secret"), "{shown}");
    }
}
