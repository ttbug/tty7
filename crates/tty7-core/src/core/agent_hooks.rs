use std::io;
use std::io::{IsTerminal as _, Read as _};
use std::path::{Path, PathBuf};

use crate::core::cli_agent::{AGENT_EVENT_SENTINEL, CLIAgent};
use crate::host::Host;

pub const TTY7_ENV_MARKER: &str = "TTY7";

const GROK_HOOK_ENV: &str = "GROK_HOOK_EVENT";

const MAX_STDIN: u64 = 64 * 1024;

pub fn run_agent_hook(agent: &str, event: &str) {
    detach_console();
    let agent = effective_agent(agent, std::env::var_os(GROK_HOOK_ENV).is_some());
    let has_tty7_marker = std::env::var_os(TTY7_ENV_MARKER).is_some();
    // MiniMax's official runner filters custom environment variables. The
    // process ancestry is the fallback proof that its command belongs to a
    // tty7 pane rather than an unrelated terminal.
    let tty7_process_owner = agent == HookAgent::MiniMaxCode.slug()
        && !has_tty7_marker
        && tty7_process_owns_hook_terminal();
    if !hook_runner_enabled(agent, has_tty7_marker, tty7_process_owner) {
        return;
    }
    let mut input = String::new();
    if !std::io::stdin().is_terminal() {
        let _ = std::io::stdin().take(MAX_STDIN).read_to_string(&mut input);
    }
    let Some(event) = effective_event(agent, event, &input) else {
        return;
    };
    write_to_controlling_tty(&build_hook_sequence(agent, event, &input));
}

#[cfg(not(unix))]
fn detach_console() {
    use windows_sys::Win32::System::Console::FreeConsole;
    unsafe {
        FreeConsole();
    }
}

#[cfg(unix)]
fn detach_console() {}

fn effective_agent(agent: &str, ran_by_grok: bool) -> &str {
    if ran_by_grok { "grok" } else { agent }
}

fn hook_runner_enabled(agent: &str, has_tty7_marker: bool, tty7_process_owner: bool) -> bool {
    has_tty7_marker || (agent == HookAgent::MiniMaxCode.slug() && tty7_process_owner)
}

#[cfg(unix)]
fn tty7_process_owns_hook_terminal() -> bool {
    use std::process::Command;

    let mut pid = unsafe { libc::getppid() };
    for _ in 0..16 {
        if pid <= 1 {
            break;
        }
        let Ok(output) = Command::new("ps")
            .args(["-o", "ppid=", "-o", "comm=", "-p", &pid.to_string()])
            .output()
        else {
            return false;
        };
        let line = String::from_utf8_lossy(&output.stdout);
        let Some((parent, name)) = parse_hook_ancestor(&line) else {
            return false;
        };
        if is_tty7_host_exe(name) {
            return true;
        }
        if parent <= 1 || parent == pid {
            break;
        }
        pid = parent;
    }
    false
}

#[cfg(any(unix, test))]
fn parse_hook_ancestor(line: &str) -> Option<(i32, &str)> {
    let (parent, name) = line.trim().split_once(char::is_whitespace)?;
    let name = name.trim_start();
    if name.is_empty() {
        return None;
    }
    Some((parent.parse().ok()?, name))
}

#[cfg(not(unix))]
fn tty7_process_owns_hook_terminal() -> bool {
    let procs = crate::daemon::winproc::snapshot();
    ancestor_pids(&procs).into_iter().any(|pid| {
        procs
            .iter()
            .find(|process| process.pid == pid)
            .is_some_and(|process| is_tty7_host_exe(&process.name))
    })
}

fn effective_event<'a>(agent: &str, event: &'a str, stdin_json: &str) -> Option<&'a str> {
    // Qoder and CodeBuddy also emit SessionStart after compacting the active
    // turn. Preserve its status until a real turn or session boundary arrives.
    if matches!(agent, "qodercli" | "codebuddy")
        && event == "session-start"
        && let Ok(payload) = serde_json::from_str::<serde_json::Value>(stdin_json)
        && payload.get("source").and_then(|value| value.as_str()) == Some("compact")
    {
        return None;
    }
    if matches!(agent, "copilot" | "grok" | "droid" | "gemini") && event == "notification" {
        let blocks = stdin_json.contains("elicitation_dialog")
            || (matches!(agent, "copilot" | "droid") && stdin_json.contains("permission_prompt"))
            // Gemini's only notification kind so far, but naming it keeps a
            // future non-blocking one from being read as a block.
            || (agent == "gemini" && stdin_json.contains("ToolPermission"));
        return blocks.then_some("permission-request");
    }
    Some(event)
}

fn build_hook_sequence(agent: &str, event: &str, stdin_json: &str) -> Vec<u8> {
    let payload: serde_json::Value =
        serde_json::from_str(stdin_json).unwrap_or(serde_json::json!({}));
    let mut body = serde_json::json!({
        "v": 1,
        "agent": agent,
        "event": event,
    });
    for (key, alias) in [
        ("session_id", "sessionId"),
        // Cursor names it after the conversation; only its `sessionStart`
        // repeats the same id as `session_id`.
        ("session_id", "conversation_id"),
        ("message", "message"),
        ("cwd", "cwd"),
        // Goose spells the working directory its own way.
        ("cwd", "working_dir"),
    ] {
        if let Some(v) = payload
            .get(key)
            .or_else(|| payload.get(alias))
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
        {
            body[key] = serde_json::Value::String(v.to_string());
        }
    }
    // Cursor puts `cwd` only on its tool events; the rest carry the workspace
    // roots instead, and the first of those is where the session runs.
    if body.get("cwd").is_none()
        && let Some(root) = payload
            .get("workspace_roots")
            .and_then(|r| r.as_array())
            .and_then(|r| r.first())
            .and_then(|r| r.as_str())
            .filter(|r| !r.is_empty())
    {
        body["cwd"] = serde_json::Value::String(root.to_string());
    }
    if let Some(prompt) = ["prompt", "userPrompt", "user_prompt"]
        .iter()
        .find_map(|k| payload.get(*k))
        .and_then(|v| v.as_str())
        .and_then(prompt_label)
    {
        body["prompt"] = serde_json::Value::String(prompt);
    }
    format!("\x1b]777;notify;{AGENT_EVENT_SENTINEL};{body}\x07").into_bytes()
}

/// How much of a prompt rides back to the terminal.
///
/// Two reasons it is short. The payload goes out as an OSC, and the tokenizer
/// reading it *abandons* anything past 8 KiB rather than truncating — a pasted
/// file would silently cost the whole event, not just its tail. And what the
/// client does with this is label one row of a list and look for that text in
/// the scrollback, neither of which can use more than a line.
const PROMPT_LABEL_MAX: usize = 200;

/// The first line of what the user typed, which is both the label an outline
/// row shows and the needle that finds the turn again in the scrollback.
///
/// A line rather than the whole prompt because the terminal wrapped it across
/// rows: a needle spanning a line break matches no single row, so the later
/// lines would only make the search fail.
fn prompt_label(text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    let end = line
        .char_indices()
        .nth(PROMPT_LABEL_MAX)
        .map_or(line.len(), |(i, _)| i);
    Some(line[..end].to_string())
}

#[cfg(unix)]
fn write_to_controlling_tty(bytes: &[u8]) -> bool {
    if write_dev(std::path::Path::new("/dev/tty"), bytes) {
        return true;
    }
    if let Some(dev) = ancestor_tty_device() {
        return write_dev(&dev, bytes);
    }
    false
}

#[cfg(unix)]
fn write_dev(path: &std::path::Path, bytes: &[u8]) -> bool {
    use std::io::Write as _;
    match std::fs::OpenOptions::new().write(true).open(path) {
        Ok(mut tty) => tty.write_all(bytes).and_then(|_| tty.flush()).is_ok(),
        Err(_) => false,
    }
}

#[cfg(unix)]
fn ancestor_tty_device() -> Option<std::path::PathBuf> {
    use std::process::Command;
    let mut pid = unsafe { libc::getppid() };
    for _ in 0..8 {
        if pid <= 1 {
            break;
        }
        let out = Command::new("ps")
            .args(["-o", "tty=", "-o", "ppid=", "-p", &pid.to_string()])
            .output()
            .ok()?;
        let line = String::from_utf8_lossy(&out.stdout);
        let mut fields = line.split_whitespace();
        let tty = fields.next().unwrap_or("");
        let ppid: i32 = fields.next().and_then(|s| s.parse().ok()).unwrap_or(1);
        if !tty.is_empty() && tty != "??" && tty != "?" {
            return Some(std::path::PathBuf::from(format!("/dev/{tty}")));
        }
        pid = ppid;
    }
    None
}

#[cfg(not(unix))]
fn write_to_controlling_tty(bytes: &[u8]) -> bool {
    let procs = crate::daemon::winproc::snapshot();
    let ancestors = ancestor_pids(&procs);

    let name_of = |pid: u32| {
        procs
            .iter()
            .find(|p| p.pid == pid)
            .map(|p| p.name.to_ascii_lowercase())
    };
    let shell = ancestors.iter().copied().find(|&pid| {
        procs
            .iter()
            .find(|p| p.pid == pid)
            .and_then(|p| name_of(p.parent))
            .is_some_and(|n| is_tty7_host_exe(&n))
    });

    if let Some(pid) = shell {
        if attach_and_write(pid, bytes) {
            return true;
        }
    }

    let mut any = false;
    for pid in ancestors {
        any |= attach_and_write(pid, bytes);
    }
    any
}

fn is_tty7_host_exe(name: &str) -> bool {
    let name = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(name)
        .to_ascii_lowercase();
    let name = name.strip_suffix(".exe").unwrap_or(&name);
    matches!(name, "tty7" | "tty7-app" | "tty7-server")
        || crate::daemon::install::asset::dialect_from_path(name).is_some()
}

#[cfg(not(unix))]
fn attach_and_write(pid: u32, bytes: &[u8]) -> bool {
    use windows_sys::Win32::System::Console::{AttachConsole, FreeConsole};
    unsafe {
        FreeConsole();
        if AttachConsole(pid) == 0 {
            return false;
        }
    }
    let ok = write_conout(bytes);
    unsafe {
        FreeConsole();
    }
    ok
}

#[cfg(not(unix))]
fn write_conout(bytes: &[u8]) -> bool {
    use std::io::Write as _;
    match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("CONOUT$")
    {
        Ok(mut out) => out.write_all(bytes).and_then(|_| out.flush()).is_ok(),
        Err(_) => false,
    }
}

#[cfg(not(unix))]
fn ancestor_pids(procs: &[crate::daemon::winproc::Proc]) -> Vec<u32> {
    let parent_of = |pid: u32| procs.iter().find(|p| p.pid == pid).map(|p| p.parent);
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut cur = std::process::id();
    seen.insert(cur);
    for _ in 0..16 {
        match parent_of(cur) {
            Some(parent) if parent != 0 && seen.insert(parent) => {
                out.push(parent);
                cur = parent;
            }
            _ => break,
        }
    }
    out
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HookAgent {
    Claude,
    Codex,
    TraeCode,
    Copilot,
    OpenCode,
    Pi,
    Grok,
    OhMyPi,
    Gemini,
    Droid,
    Qwen,
    Goose,
    Kimi,
    QoderCLI,
    Crush,
    CommandCode,
    MiniMaxCode,
    CodeBuddy,
    Cursor,
}

impl HookAgent {
    pub const ALL: [HookAgent; 19] = [
        HookAgent::Claude,
        HookAgent::Codex,
        HookAgent::TraeCode,
        HookAgent::Copilot,
        HookAgent::OpenCode,
        HookAgent::Pi,
        HookAgent::Grok,
        HookAgent::OhMyPi,
        HookAgent::Gemini,
        HookAgent::Droid,
        HookAgent::Qwen,
        HookAgent::Goose,
        HookAgent::Kimi,
        HookAgent::QoderCLI,
        HookAgent::Crush,
        HookAgent::CommandCode,
        HookAgent::MiniMaxCode,
        HookAgent::CodeBuddy,
        HookAgent::Cursor,
    ];

    /// The hooks behind a detected agent process, if it has any.
    ///
    /// Process detection knows far more agents than hooks do — the ones with no
    /// arm here report status some other way, or not at all. The match is
    /// exhaustive on purpose: a newly detected agent has to say which it is.
    pub fn of_detected(agent: CLIAgent) -> Option<HookAgent> {
        match agent {
            CLIAgent::Claude => Some(HookAgent::Claude),
            CLIAgent::Codex => Some(HookAgent::Codex),
            CLIAgent::TraeCode => Some(HookAgent::TraeCode),
            CLIAgent::Copilot => Some(HookAgent::Copilot),
            CLIAgent::OpenCode => Some(HookAgent::OpenCode),
            CLIAgent::Pi => Some(HookAgent::Pi),
            CLIAgent::Grok => Some(HookAgent::Grok),
            CLIAgent::OhMyPi => Some(HookAgent::OhMyPi),
            CLIAgent::Gemini => Some(HookAgent::Gemini),
            CLIAgent::Droid => Some(HookAgent::Droid),
            CLIAgent::Qwen => Some(HookAgent::Qwen),
            CLIAgent::Goose => Some(HookAgent::Goose),
            CLIAgent::Kimi => Some(HookAgent::Kimi),
            CLIAgent::QoderCLI => Some(HookAgent::QoderCLI),
            CLIAgent::Crush => Some(HookAgent::Crush),
            CLIAgent::CommandCode => Some(HookAgent::CommandCode),
            CLIAgent::MiniMaxCode => Some(HookAgent::MiniMaxCode),
            CLIAgent::CodeBuddy => Some(HookAgent::CodeBuddy),
            CLIAgent::Cursor => Some(HookAgent::Cursor),
            CLIAgent::Aider
            | CLIAgent::Amp
            | CLIAgent::Auggie
            | CLIAgent::Hermes
            | CLIAgent::Vibe
            | CLIAgent::Antigravity => None,
        }
    }

    /// The events this agent's hooks merge into a shared JSON config, if that
    /// is how it takes them. `None` means the agent owns a generated file
    /// or a native Plugin package instead.
    fn hook_map_events(self) -> Option<&'static [(&'static str, &'static str)]> {
        match self {
            HookAgent::Claude => Some(CLAUDE_HOOK_EVENTS),
            HookAgent::Codex => Some(CODEX_HOOK_EVENTS),
            HookAgent::TraeCode => Some(TRAE_CODE_HOOK_EVENTS),
            HookAgent::Gemini => Some(GEMINI_HOOK_EVENTS),
            HookAgent::Droid => Some(DROID_HOOK_EVENTS),
            HookAgent::Qwen => Some(QWEN_HOOK_EVENTS),
            HookAgent::QoderCLI => Some(QODER_HOOK_EVENTS),
            HookAgent::Crush => Some(CRUSH_HOOK_EVENTS),
            HookAgent::CommandCode => Some(COMMANDCODE_HOOK_EVENTS),
            HookAgent::CodeBuddy => Some(CODEBUDDY_HOOK_EVENTS),
            HookAgent::Cursor => Some(CURSOR_HOOK_EVENTS),
            HookAgent::Copilot
            | HookAgent::OpenCode
            | HookAgent::Pi
            | HookAgent::Grok
            | HookAgent::OhMyPi
            | HookAgent::Goose
            | HookAgent::Kimi
            | HookAgent::MiniMaxCode => None,
        }
    }

    /// The events this agent's hooks merge into a shared TOML config, if that
    /// is how it takes them — the third strategy, for the agents whose hooks
    /// live as `[[hooks]]` entries in a config file the user also hand-edits.
    fn toml_hook_events(self) -> Option<&'static [(&'static str, &'static str)]> {
        match self {
            HookAgent::Kimi => Some(KIMI_HOOK_EVENTS),
            _ => None,
        }
    }

    /// Whether this agent's hook-map entries carry `command` and `matcher` at
    /// the top level rather than nesting a `hooks` array of `{type, command}`
    /// objects inside the matcher. Claude's shape is the latter; Crush flattened
    /// it, and still calls itself Claude-Code-compatible on the wire. Cursor's
    /// `hooks.json` was flat from the start.
    fn flat_hook_map(self) -> bool {
        matches!(self, HookAgent::Crush | HookAgent::Cursor)
    }

    /// The schema version a hook map has to declare at its root, if the agent
    /// demands one. Cursor ignores a `hooks.json` without `"version": 1`, so a
    /// file tty7 creates — or finds without one — gets it; one the user already
    /// versioned is left alone.
    fn hook_map_version(self) -> Option<u64> {
        match self {
            HookAgent::Cursor => Some(1),
            _ => None,
        }
    }

    pub fn slug(self) -> &'static str {
        match self {
            HookAgent::Claude => "claude",
            HookAgent::Codex => "codex",
            HookAgent::TraeCode => "traecli",
            HookAgent::Copilot => "copilot",
            HookAgent::OpenCode => "opencode",
            HookAgent::Pi => "pi",
            HookAgent::Grok => "grok",
            HookAgent::OhMyPi => "omp",
            HookAgent::Gemini => "gemini",
            HookAgent::Droid => "droid",
            HookAgent::Qwen => "qwen",
            HookAgent::Goose => "goose",
            HookAgent::Kimi => "kimi",
            HookAgent::QoderCLI => "qodercli",
            HookAgent::Crush => "crush",
            HookAgent::CommandCode => "command-code",
            HookAgent::MiniMaxCode => "minimax-code",
            HookAgent::CodeBuddy => "codebuddy",
            HookAgent::Cursor => "cursor",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            HookAgent::Claude => "Claude Code",
            HookAgent::Codex => "Codex",
            HookAgent::TraeCode => "TraeCode",
            HookAgent::Copilot => "Copilot CLI",
            HookAgent::OpenCode => "OpenCode",
            HookAgent::Pi => "Pi",
            HookAgent::Grok => "Grok Build",
            HookAgent::OhMyPi => "Oh My Pi",
            HookAgent::Gemini => "Gemini",
            HookAgent::Droid => "Droid",
            HookAgent::Qwen => "Qwen Code",
            HookAgent::Goose => "Goose",
            HookAgent::Kimi => "Kimi Code",
            HookAgent::QoderCLI => "Qoder CLI",
            HookAgent::Crush => "Crush",
            HookAgent::CommandCode => "Command Code",
            HookAgent::MiniMaxCode => "MiniMax Code",
            HookAgent::CodeBuddy => "CodeBuddy",
            HookAgent::Cursor => "Cursor CLI",
        }
    }

    pub fn target_display(self, target: &HookTarget) -> String {
        let path = if self == HookAgent::MiniMaxCode {
            target.minimax_plugin_root()
        } else {
            self.target_path(target)
        };
        target.abbreviate_home(&path)
    }

    fn target_path(self, target: &HookTarget) -> PathBuf {
        match self {
            HookAgent::Claude => target.claude_settings_path(),
            HookAgent::Codex => target.under_home(&[".codex", "hooks.json"]),
            HookAgent::TraeCode => target.traecli_hooks_path(),
            HookAgent::Copilot => target.under_home(&[".copilot", "hooks", OWNED_FILE_STEM_JSON]),
            HookAgent::OpenCode => target.under(
                &target.xdg_config_dir(),
                &["opencode", "plugins", OWNED_FILE_STEM_JS],
            ),
            HookAgent::Pi => target.under_home(&[".pi", "agent", "extensions", "tty7", "index.ts"]),
            HookAgent::Grok => target.under_home(&[".grok", "hooks", OWNED_FILE_STEM_JSON]),
            HookAgent::OhMyPi => {
                target.under_home(&[".omp", "agent", "extensions", "tty7", "index.ts"])
            }
            HookAgent::Gemini => target.under_home(&[".gemini", "settings.json"]),
            HookAgent::Droid => target.under_home(&[".factory", "settings.json"]),
            HookAgent::Qwen => target.under_home(&[".qwen", "settings.json"]),
            // The Open Plugins layout, which Goose implements rather than
            // inventing its own: any `.agents/plugins/<name>/hooks/hooks.json`
            // is picked up at startup.
            HookAgent::Goose => {
                target.under_home(&[".agents", "plugins", "tty7", "hooks", "hooks.json"])
            }
            HookAgent::Kimi => target.kimi_config_path(),
            HookAgent::QoderCLI => target.qoder_settings_path(),
            HookAgent::Crush => target.crush_settings_path(),
            // Claude's hook shape in a fixed home path; the docs name no
            // config-dir override for it.
            HookAgent::CommandCode => target.under_home(&[".commandcode", "settings.json"]),
            HookAgent::MiniMaxCode => target.minimax_plugin_manifest_path(),
            HookAgent::CodeBuddy => target.codebuddy_settings_path(),
            // The user-level file; Cursor merges it under any project one.
            HookAgent::Cursor => target.under_home(&[".cursor", "hooks.json"]),
        }
    }

    fn marker(self) -> String {
        format!("agent-hook {}", self.slug())
    }
}

const MAX_CONFIG_BYTES: u64 = 4 * 1024 * 1024;

const MINIMAX_PLUGIN_NAME: &str = "tty7-agent-hooks";
const MINIMAX_PLUGIN_VERSION: &str = "1.0.0";
const MINIMAX_PLUGIN_AUTHOR: &str = "tty7";
const MINIMAX_PLUGIN_DESCRIPTION: &str =
    "tty7 lifecycle status hooks for MiniMax Code. Generated by tty7.";
const MINIMAX_PLUGIN_ICON: &[u8] = include_bytes!("../../../../assets/logo@256.png");
const MINIMAX_PLUGIN_MANIFEST_TEMP: &str = ".plugin.json.tty7.tmp";
const MINIMAX_PLUGIN_HOOKS_MARKER: &str = "tty7-agent-hooks-v1";
const MINIMAX_PLUGIN_HOOKS_TEMP: &str = ".tty7-hooks.json.tmp";
const MINIMAX_PLUGIN_ICON_TEMP: &str = ".tty7-icon.png.tmp";

pub struct HookTarget<'a> {
    host: &'a dyn Host,
    home: PathBuf,
    exe: PathBuf,
}

impl<'a> HookTarget<'a> {
    pub fn local(host: &'a dyn Host) -> Option<HookTarget<'a>> {
        Self::local_for_exe(host, std::env::current_exe().ok()?)
    }

    /// Build a local target for a known tty7 hook runner.
    ///
    /// Most callers use [`Self::local`]. The standalone CLI is the exception:
    /// it diagnoses hooks but never executes them, so it supplies the
    /// `tty7-app` it would launch instead of comparing configs to `tty7`.
    pub fn local_for_exe(host: &'a dyn Host, exe: PathBuf) -> Option<HookTarget<'a>> {
        Some(HookTarget {
            host,
            home: home_dir()?,
            exe,
        })
    }

    pub fn remote(host: &'a dyn Host, home: PathBuf) -> HookTarget<'a> {
        let dialect = crate::daemon::install::RemoteProtocol::of_this_build();
        let binary = crate::daemon::install::asset::remote_paths(
            &home.to_string_lossy(),
            dialect.control,
            dialect.protocol,
        )
        .binary;
        HookTarget {
            host,
            home,
            exe: PathBuf::from(binary),
        }
    }

    fn is_local(&self) -> bool {
        self.host.id().is_local()
    }

    fn under(&self, base: &Path, parts: &[&str]) -> PathBuf {
        let mut p = base.to_path_buf();
        for part in parts {
            p = self.host.join(&p, part);
        }
        p
    }

    fn under_home(&self, parts: &[&str]) -> PathBuf {
        self.under(&self.home, parts)
    }

    fn minimax_data_dir(&self) -> PathBuf {
        let primary = std::env::var_os("MINIMAX_DATA_DIR");
        let legacy = std::env::var_os("MAVIS_DATA_DIR");
        minimax_data_dir_for(
            &self.home,
            self.is_local(),
            primary.as_deref(),
            legacy.as_deref(),
        )
    }

    fn minimax_plugin_root(&self) -> PathBuf {
        self.under(&self.minimax_data_dir(), &["plugins", MINIMAX_PLUGIN_NAME])
    }

    fn minimax_plugin_manifest_path(&self) -> PathBuf {
        self.under(
            &self.minimax_plugin_root(),
            &[".minimax-plugin", "plugin.json"],
        )
    }

    fn minimax_plugin_hooks_path(&self) -> PathBuf {
        self.under(&self.minimax_plugin_root(), &["hooks", "tty7.json"])
    }

    fn minimax_plugin_icon_path(&self) -> PathBuf {
        self.under(&self.minimax_plugin_root(), &["icon.png"])
    }

    fn minimax_plugin_manifest_temp_path(&self) -> PathBuf {
        self.under(
            &self.minimax_plugin_root(),
            &[".minimax-plugin", MINIMAX_PLUGIN_MANIFEST_TEMP],
        )
    }

    fn minimax_plugin_hooks_temp_path(&self) -> PathBuf {
        self.under(&self.minimax_plugin_root(), &[MINIMAX_PLUGIN_HOOKS_TEMP])
    }

    fn minimax_plugin_icon_temp_path(&self) -> PathBuf {
        self.under(&self.minimax_plugin_root(), &[MINIMAX_PLUGIN_ICON_TEMP])
    }

    fn minimax_plugin_guard_paths(&self) -> [PathBuf; 11] {
        let data_dir = self.minimax_data_dir();
        let plugins = self.under(&data_dir, &["plugins"]);
        let root = self.minimax_plugin_root();
        [
            data_dir,
            plugins,
            root.clone(),
            self.under(&root, &[".minimax-plugin"]),
            self.under(&root, &["hooks"]),
            self.minimax_plugin_manifest_path(),
            self.minimax_plugin_manifest_temp_path(),
            self.minimax_plugin_hooks_path(),
            self.minimax_plugin_icon_path(),
            self.minimax_plugin_hooks_temp_path(),
            self.minimax_plugin_icon_temp_path(),
        ]
    }

    fn claude_settings_path(&self) -> PathBuf {
        if self.is_local()
            && let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|d| !d.is_empty())
        {
            return PathBuf::from(dir).join("settings.json");
        }
        self.under_home(&[".claude", "settings.json"])
    }

    fn xdg_config_dir(&self) -> PathBuf {
        if self.is_local()
            && let Some(dir) = std::env::var_os("XDG_CONFIG_HOME").filter(|d| !d.is_empty())
        {
            return PathBuf::from(dir);
        }
        self.under_home(&[".config"])
    }

    fn kimi_config_path(&self) -> PathBuf {
        if self.is_local()
            && let Some(dir) = std::env::var_os("KIMI_CODE_HOME").filter(|d| !d.is_empty())
        {
            return PathBuf::from(dir).join("config.toml");
        }
        self.under_home(&[".kimi-code", "config.toml"])
    }

    fn qoder_settings_path(&self) -> PathBuf {
        if self.is_local()
            && let Some(dir) = std::env::var_os("QODER_CONFIG_DIR").filter(|d| !d.is_empty())
        {
            return PathBuf::from(dir).join("settings.json");
        }
        self.under_home(&[".qoder", "settings.json"])
    }

    /// The global `crush.json`. Crush resolves it through `CRUSH_GLOBAL_CONFIG`
    /// when set, otherwise `$XDG_CONFIG_HOME/crush/crush.json` (and `~/.config`
    /// when that is unset) — which is exactly what [`Self::xdg_config_dir`]
    /// answers. The override is local-only: a local env var must not redirect a
    /// remote machine's hooks.
    fn crush_settings_path(&self) -> PathBuf {
        if self.is_local()
            && let Some(dir) = std::env::var_os("CRUSH_GLOBAL_CONFIG").filter(|d| !d.is_empty())
        {
            return PathBuf::from(dir).join("crush.json");
        }
        self.under(&self.xdg_config_dir(), &["crush", "crush.json"])
    }

    /// CodeBuddy moves its whole home, `settings.json` included, to
    /// `CODEBUDDY_CONFIG_DIR` when that is set and not blank. Local-only, like
    /// the other overrides.
    fn codebuddy_settings_path(&self) -> PathBuf {
        if self.is_local()
            && let Some(dir) = std::env::var_os("CODEBUDDY_CONFIG_DIR")
                .filter(|d| !d.to_string_lossy().trim().is_empty())
        {
            return PathBuf::from(dir).join("settings.json");
        }
        self.under_home(&[".codebuddy", "settings.json"])
    }

    fn traecli_hooks_path(&self) -> PathBuf {
        if self.is_local() {
            if let Some(dir) = std::env::var_os("TRAECLI_HOME").filter(|d| !d.is_empty()) {
                return PathBuf::from(dir).join("hooks.json");
            }
            if let Some(dir) = std::env::var_os("TRAE_HOME").filter(|d| !d.is_empty()) {
                return PathBuf::from(dir).join("cli").join("hooks.json");
            }
        }
        self.under_home(&[".trae", "cli", "hooks.json"])
    }

    fn hook_command(&self, agent: HookAgent, event: &str) -> String {
        if let Some(exe) = self.hook_command_exe() {
            return format!("{exe} agent-hook {} {event}", agent.slug());
        }
        format!(
            "\"{}\" agent-hook {} {event}",
            self.exe.display(),
            agent.slug()
        )
    }

    /// A shell-safe executable path for generated hook commands.
    ///
    /// On Windows, Codex runs hook commands through the session's shell, which
    /// is frequently PowerShell. `pwsh -Command` drops the quotes around a
    /// path containing spaces, so the usual `"C:\Program Files\..."` form is
    /// parsed as `C:\Program` and fails — and even a quoted path without
    /// spaces is a syntax error in PowerShell (invoking a quoted path requires
    /// the `&` call operator). When the executable resolves by its bare file
    /// name from PATH, we can emit the name without any quoting, which every
    /// shell (`cmd.exe`, PowerShell, bash) executes correctly.
    ///
    /// Returns `None` when the executable is not resolvable by name from PATH,
    /// or when the first PATH match is a different binary; callers then fall
    /// back to the quoted full path.
    fn hook_command_exe(&self) -> Option<String> {
        #[cfg(windows)]
        if self.is_local() {
            if let Some(name) = path_resolvable_name(&self.exe) {
                return Some(name);
            }
        }
        None
    }

    fn read(&self, p: &Path) -> io::Result<String> {
        let bytes = self.host.read_file(p, MAX_CONFIG_BYTES)?;
        String::from_utf8(bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    fn write(&self, p: &Path, bytes: &[u8]) -> anyhow::Result<()> {
        if let Some(parent) = p.parent() {
            self.host.create_dir(parent, true)?;
        }
        if self.is_local() {
            crate::core::config::write_atomic(p, bytes)?;
        } else {
            self.host.write_file(p, bytes)?;
        }
        Ok(())
    }

    fn abbreviate_home(&self, path: &Path) -> String {
        match path.strip_prefix(&self.home) {
            Ok(rest) => format!("~/{}", rest.display()),
            Err(_) => path.display().to_string(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HooksState {
    NotInstalled,
    Installed,
    Outdated,
}

pub fn hooks_state(target: &HookTarget, agent: HookAgent) -> HooksState {
    if agent == HookAgent::MiniMaxCode {
        return minimax_plugin_state(target);
    }
    let path = agent.target_path(target);
    if let Some(events) = agent.toml_hook_events() {
        return toml_hooks_state(target, &path, agent, events);
    }
    if let Some(events) = agent.hook_map_events() {
        return hook_map_state(target, &path, agent, events);
    }
    let Some(expected) = owned_file_content(target, agent) else {
        return HooksState::NotInstalled;
    };
    owned_file_state(target, &path, &expected, &agent.marker())
}

/// What an install or uninstall actually did.
///
/// These land in a note the user reads in Settings, and this crate cannot
/// reach `src/ui/i18n`, which owns every user-visible string. Returning a
/// sentence from here meant a Chinese or Japanese UI reported "Installed" in
/// English; returning the outcome lets the caller word it in the user's
/// language.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookOutcome {
    Installed,
    /// Installed on a remote, where `codex features enable hooks` still has to
    /// be run once by hand.
    InstalledEnableCodexThere,
    /// Installed, but running `codex features enable hooks` here failed.
    InstalledCodexEnableFailed(String),
    Removed,
    /// The agent has no config file at all.
    NothingInstalled,
    /// The config is there, but holds no tty7 hooks.
    NoTty7Hooks,
}

pub fn install_hooks(target: &HookTarget, agent: HookAgent) -> anyhow::Result<HookOutcome> {
    if agent == HookAgent::MiniMaxCode {
        minimax_plugin_install(target)?;
        return Ok(HookOutcome::Installed);
    }
    let path = agent.target_path(target);
    if let Some(events) = agent.toml_hook_events() {
        toml_hooks_install(target, &path, agent, events)?;
        return Ok(HookOutcome::Installed);
    }
    if let Some(events) = agent.hook_map_events() {
        hook_map_install(target, &path, agent, events)?;
        if agent != HookAgent::Codex {
            return Ok(HookOutcome::Installed);
        }
        if !target.is_local() {
            return Ok(HookOutcome::InstalledEnableCodexThere);
        }
        return Ok(match enable_codex_hooks_feature() {
            Ok(()) => HookOutcome::Installed,
            Err(e) => HookOutcome::InstalledCodexEnableFailed(e.to_string()),
        });
    }
    let content = owned_file_content(target, agent)
        .ok_or_else(|| anyhow::anyhow!("{agent:?} has no owned file"))?;
    owned_file_install(target, &path, &content, &agent.marker())?;
    Ok(HookOutcome::Installed)
}

pub fn uninstall_hooks(target: &HookTarget, agent: HookAgent) -> anyhow::Result<HookOutcome> {
    if agent == HookAgent::MiniMaxCode {
        return minimax_plugin_uninstall(target);
    }
    let path = agent.target_path(target);
    if agent.toml_hook_events().is_some() {
        return toml_hooks_uninstall(target, &path, agent);
    }
    match agent.hook_map_events() {
        Some(_) => hook_map_uninstall(target, &path, agent),
        None => owned_file_uninstall(target, &path, &agent.marker()),
    }
}

pub fn refresh_hooks(target: &HookTarget) -> usize {
    let mut refreshed = 0;
    for agent in HookAgent::ALL {
        if hooks_state(target, agent) != HooksState::Outdated {
            continue;
        }
        match install_hooks(target, agent) {
            Ok(summary) => {
                refreshed += 1;
                log::info!(
                    "refreshed stale {} hooks at {}: {summary:?}",
                    agent.display_name(),
                    agent.target_display(target)
                );
            }
            Err(e) => log::warn!(
                "could not refresh stale {} hooks: {e}",
                agent.display_name()
            ),
        }
    }
    refreshed
}

pub fn refresh_remote_hooks(host: &dyn Host, home: PathBuf) -> usize {
    if home_dir().is_some_and(|ours| ours == home) {
        return 0;
    }
    refresh_hooks(&HookTarget::remote(host, home))
}

pub fn refresh_hooks_at_launch() -> usize {
    if cfg!(debug_assertions) {
        return 0;
    }
    let host = crate::host::local::LocalHost::new();
    let Some(target) = HookTarget::local(&*host) else {
        return 0;
    };
    refresh_hooks(&target)
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
    #[cfg(not(unix))]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
}

fn minimax_data_dir_for(
    home: &Path,
    local: bool,
    primary: Option<&std::ffi::OsStr>,
    legacy: Option<&std::ffi::OsStr>,
) -> PathBuf {
    if local {
        if let Some(dir) = primary.filter(|dir| !dir.is_empty()) {
            return PathBuf::from(dir);
        }
        if let Some(dir) = legacy.filter(|dir| !dir.is_empty()) {
            return PathBuf::from(dir);
        }
    }
    let mut path = home.to_path_buf();
    path.push(".minimax");
    path
}

const OWNED_FILE_STEM_JSON: &str = "tty7.json";
const OWNED_FILE_STEM_JS: &str = "tty7.js";

const CLAUDE_HOOK_EVENTS: &[(&str, &str)] = &[
    ("SessionStart", "session-start"),
    ("UserPromptSubmit", "prompt-submit"),
    ("Notification", "notification"),
    ("PostToolUse", "tool-complete"),
    ("Stop", "stop"),
    ("SessionEnd", "session-end"),
];

const CODEX_HOOK_EVENTS: &[(&str, &str)] = &[
    ("SessionStart", "session-start"),
    ("UserPromptSubmit", "prompt-submit"),
    ("Stop", "stop"),
];

const TRAE_CODE_HOOK_EVENTS: &[(&str, &str)] = &[
    ("SessionStart", "session-start"),
    ("UserPromptSubmit", "prompt-submit"),
    ("PermissionRequest", "permission-request"),
    ("PostToolUse", "tool-complete"),
    ("Stop", "stop"),
    ("SessionEnd", "session-end"),
];

/// Gemini names the turn boundaries after the agent rather than the user, and
/// omitting `matcher` matches everything (`hookPlanner.ts`, `!entry.matcher`),
/// so the bare entries [`hook_map_install`] already writes are enough.
const GEMINI_HOOK_EVENTS: &[(&str, &str)] = &[
    ("SessionStart", "session-start"),
    ("BeforeAgent", "prompt-submit"),
    ("Notification", "notification"),
    ("AfterTool", "tool-complete"),
    ("AfterAgent", "stop"),
    ("SessionEnd", "session-end"),
];

const DROID_HOOK_EVENTS: &[(&str, &str)] = &[
    ("SessionStart", "session-start"),
    ("UserPromptSubmit", "prompt-submit"),
    ("Notification", "notification"),
    ("PostToolUse", "tool-complete"),
    ("Stop", "stop"),
    ("SessionEnd", "session-end"),
];

/// Qwen is the only agent here with a first-class permission event, so it needs
/// none of the notification sniffing in [`effective_event`] — and it gets no
/// `Notification` hook at all, which would only muddy a status the dedicated
/// event already reports precisely.
const QWEN_HOOK_EVENTS: &[(&str, &str)] = &[
    ("SessionStart", "session-start"),
    ("UserPromptSubmit", "prompt-submit"),
    ("PermissionRequest", "permission-request"),
    ("PostToolUse", "tool-complete"),
    ("Stop", "stop"),
    ("SessionEnd", "session-end"),
];

/// Kimi Code's hooks live as `[[hooks]]` entries in its main `config.toml` —
/// the same file that holds the user's providers and models — so they go
/// through the TOML merge strategy rather than a JSON map or an owned file.
/// Like Qwen it has a first-class permission event, so it needs no
/// `Notification` hook and none of the sniffing in [`effective_event`].
///
/// `Stop` alone does not cover every way a turn ends here: Kimi's own event
/// reference says `Stop` "does not fire on interrupts, so this event fires
/// instead" of `Interrupt`, and a turn that dies on an error reports
/// `StopFailure`. Without those two an <kbd>Esc</kbd> or a failed turn would
/// leave the pane on "working" forever and `tty7 wait` would only ever time
/// out, so both report the same end-of-turn as `Stop` does. All three are
/// observation-only events, and a doubled `stop` is idempotent.
const KIMI_HOOK_EVENTS: &[(&str, &str)] = &[
    ("SessionStart", "session-start"),
    ("UserPromptSubmit", "prompt-submit"),
    ("PermissionRequest", "permission-request"),
    ("PostToolUse", "tool-complete"),
    ("Stop", "stop"),
    ("Interrupt", "stop"),
    ("StopFailure", "stop"),
    ("SessionEnd", "session-end"),
];

const GROK_HOOK_TIMEOUT_SECS: u32 = 10;

const GROK_HOOK_EVENTS: &[(&str, &str, Option<&str>)] = &[
    ("SessionStart", "session-start", None),
    ("UserPromptSubmit", "prompt-submit", None),
    ("Notification", "notification", Some("elicitation_dialog")),
    ("PostToolUse", "tool-complete", None),
    ("Stop", "stop", None),
    ("SessionEnd", "session-end", None),
];

const QODER_HOOK_EVENTS: &[(&str, &str)] = &[
    ("SessionStart", "session-start"),
    ("UserPromptSubmit", "prompt-submit"),
    ("PermissionRequest", "permission-request"),
    // An authorized MCP tool can still pause for user input mid-call.
    ("Elicitation", "question-asked"),
    ("PostToolUse", "tool-complete"),
    ("Stop", "stop"),
    ("StopFailure", "stop"),
    ("SessionEnd", "session-end"),
];

/// CodeBuddy's hooks are Claude Code's, file layout and event names alike, but
/// like Qoder it has a first-class `PermissionRequest`, so it takes Qoder's
/// table rather than Claude's `Notification` sniffing. A turn that dies on an
/// API error reports `StopFailure` instead of `Stop`; without it the pane
/// would stay on working.
const CODEBUDDY_HOOK_EVENTS: &[(&str, &str)] = &[
    ("SessionStart", "session-start"),
    ("UserPromptSubmit", "prompt-submit"),
    ("PermissionRequest", "permission-request"),
    // An authorized MCP tool can still pause for user input mid-call.
    ("Elicitation", "question-asked"),
    ("PostToolUse", "tool-complete"),
    ("Stop", "stop"),
    ("StopFailure", "stop"),
    ("SessionEnd", "session-end"),
];

/// Crush currently fires exactly one hook, `PreToolUse`, before every
/// top-level tool call and before its permission check. Its payload still
/// carries `session_id` and `cwd`, which is what resume needs.
///
/// It maps to `tool-complete`, not `prompt-submit`: with no `Stop` to follow,
/// a turn started here would never end, and a pane stuck on working asks
/// before every close and holds the tray and `tty7 wait` on a turn long over.
/// `tool-complete` records the session and bumps the activity counter while
/// leaving the status alone. When Crush ships `UserPromptSubmit`/`Stop` and
/// friends, they slot in here.
const CRUSH_HOOK_EVENTS: &[(&str, &str)] = &[("PreToolUse", "tool-complete")];

/// Command Code mirrors Claude's nested `hooks` map in
/// `~/.commandcode/settings.json`, but ships only four events:
/// SessionStart, PreToolUse, PostToolUse and Stop. There is no
/// UserPromptSubmit and no SessionEnd. PreToolUse is therefore the first
/// observable boundary of a new agent turn: treating it as prompt-submit
/// moves a pane out of Done before the tool runs. A text-only turn still has
/// no start event, but a tool-using turn no longer spends its whole run
/// labelled complete. PostToolUse keeps the activity counter moving until
/// Stop closes the turn.
const COMMANDCODE_HOOK_EVENTS: &[(&str, &str)] = &[
    ("SessionStart", "session-start"),
    ("PreToolUse", "prompt-submit"),
    ("PostToolUse", "tool-complete"),
    ("Stop", "stop"),
];

/// MiniMax Code loads these entries from a native multi-file Plugin package.
/// Its public hook contract has explicit turn, permission and session events,
/// so there is no need to infer permissions from a generic notification.
const MINIMAX_HOOK_EVENTS: &[(&str, &str)] = &[
    ("SessionStart", "session-start"),
    ("UserPromptSubmit", "prompt-submit"),
    ("PermissionRequest", "permission-request"),
    ("PostToolUse", "tool-complete"),
    ("Stop", "stop"),
    ("SessionEnd", "session-end"),
];

fn minimax_plugin_state(target: &HookTarget) -> HooksState {
    if minimax_reject_plugin_symlinks(target).is_err() {
        return HooksState::NotInstalled;
    }
    let manifest_path = target.minimax_plugin_manifest_path();
    let Ok(manifest) = minimax_read_text(target, &manifest_path) else {
        return HooksState::NotInstalled;
    };
    if !minimax_manifest_is_ours(&manifest) {
        return HooksState::NotInstalled;
    }

    let Ok(expected_manifest) = minimax_plugin_manifest() else {
        return HooksState::Outdated;
    };
    let Ok(expected_hooks) = minimax_plugin_hooks(target) else {
        return HooksState::Outdated;
    };
    let hooks = minimax_read_text(target, &target.minimax_plugin_hooks_path()).ok();
    let icon = minimax_read_bytes(target, &target.minimax_plugin_icon_path()).ok();
    if manifest == expected_manifest
        && hooks.as_deref() == Some(expected_hooks.as_str())
        && icon.as_deref() == Some(MINIMAX_PLUGIN_ICON)
    {
        HooksState::Installed
    } else {
        HooksState::Outdated
    }
}

fn minimax_plugin_install(target: &HookTarget) -> anyhow::Result<()> {
    minimax_reject_plugin_symlinks(target)?;
    let root = target.minimax_plugin_root();
    let manifest_path = target.minimax_plugin_manifest_path();
    let hooks_path = target.minimax_plugin_hooks_path();
    let icon_path = target.minimax_plugin_icon_path();
    let manifest_temp_path = target.minimax_plugin_manifest_temp_path();
    let hooks_temp_path = target.minimax_plugin_hooks_temp_path();
    let icon_temp_path = target.minimax_plugin_icon_temp_path();
    let manifest = minimax_plugin_manifest()?;
    let hooks = minimax_plugin_hooks(target)?;

    let root_exists = match target.host.stat(&root) {
        Ok(meta) => {
            if !meta.is_dir {
                return Err(anyhow::anyhow!(
                    "{} exists but is not a directory; not touching it",
                    root.display()
                ));
            }
            true
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => false,
        Err(e) => return Err(anyhow::anyhow!("read {}: {e}", root.display())),
    };

    let manifest_owned = match minimax_read_text(target, &manifest_path) {
        Ok(manifest) if !minimax_manifest_is_ours(&manifest) => {
            return Err(anyhow::anyhow!(
                "{} exists but wasn't written by tty7; not touching it",
                manifest_path.display()
            ));
        }
        Ok(_) => true,
        Err(e) if e.kind() == io::ErrorKind::NotFound && root_exists => {
            if !minimax_partial_package_is_ours(target, &hooks, &manifest)? {
                return Err(anyhow::anyhow!(
                    "{} exists without a tty7 manifest; not touching it",
                    root.display()
                ));
            }
            false
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => false,
        Err(e) => return Err(anyhow::anyhow!("read {}: {e}", manifest_path.display())),
    };

    match minimax_read_text(target, &hooks_path) {
        Ok(hooks) if !minimax_hooks_contain_ownership_marker(&hooks) => {
            return Err(anyhow::anyhow!(
                "{} exists but wasn't written by tty7; not touching it",
                hooks_path.display()
            ));
        }
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(anyhow::anyhow!("read {}: {e}", hooks_path.display())),
    }

    let result = (|| {
        // Keep every package file behind a completed temporary write. This is
        // particularly important for remote hosts, where Host::write_file is
        // a direct server-side write and can otherwise leave a truncated
        // hooks or icon file that blocks the next install.
        minimax_publish_file(target, &hooks_path, &hooks_temp_path, hooks.as_bytes())?;
        minimax_publish_file(target, &icon_path, &icon_temp_path, MINIMAX_PLUGIN_ICON)?;
        minimax_publish_file(
            target,
            &manifest_path,
            &manifest_temp_path,
            manifest.as_bytes(),
        )?;
        Ok(())
    })();
    if result.is_err() {
        for path in [&hooks_temp_path, &icon_temp_path, &manifest_temp_path] {
            let _ = target.host.remove(path, false);
        }
        if !manifest_owned {
            minimax_cleanup_partial_package(target, &hooks);
        }
    }
    result
}

fn minimax_plugin_uninstall(target: &HookTarget) -> anyhow::Result<HookOutcome> {
    minimax_reject_plugin_symlinks(target)?;
    let root = target.minimax_plugin_root();
    let manifest_path = target.minimax_plugin_manifest_path();
    let hooks_path = target.minimax_plugin_hooks_path();
    let icon_path = target.minimax_plugin_icon_path();
    let hooks_temp_path = target.minimax_plugin_hooks_temp_path();
    let icon_temp_path = target.minimax_plugin_icon_temp_path();
    let manifest_temp_path = target.minimax_plugin_manifest_temp_path();

    match target.host.stat(&root) {
        Ok(meta) if !meta.is_dir => {
            return Err(anyhow::anyhow!(
                "{} is not a directory; not touching it",
                root.display()
            ));
        }
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Ok(HookOutcome::NothingInstalled);
        }
        Err(e) => return Err(anyhow::anyhow!("read {}: {e}", root.display())),
    }

    let manifest = match minimax_read_text(target, &manifest_path) {
        Ok(manifest) => manifest,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Ok(HookOutcome::NothingInstalled);
        }
        Err(e) => return Err(anyhow::anyhow!("read {}: {e}", manifest_path.display())),
    };
    if !minimax_manifest_is_ours(&manifest) {
        return Err(anyhow::anyhow!(
            "{} wasn't written by tty7; not touching it",
            manifest_path.display()
        ));
    }

    let hooks = match minimax_read_text(target, &hooks_path) {
        Ok(hooks) => Some(hooks),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(anyhow::anyhow!("read {}: {e}", hooks_path.display())),
    };
    if hooks
        .as_deref()
        .is_some_and(|text| !minimax_hooks_contain_ownership_marker(text))
    {
        return Err(anyhow::anyhow!(
            "{} wasn't written by tty7; not touching it",
            hooks_path.display()
        ));
    }

    let remove_icon = minimax_read_bytes(target, &icon_path)
        .ok()
        .is_some_and(|bytes| bytes == MINIMAX_PLUGIN_ICON);

    if hooks.is_some() {
        target.host.remove(&hooks_path, false)?;
    }
    target.host.remove(&manifest_path, false)?;
    for path in [&hooks_temp_path, &icon_temp_path, &manifest_temp_path] {
        let _ = target.host.remove(path, false);
    }
    if remove_icon {
        target.host.remove(&icon_path, false)?;
    }

    for dir in [
        target.under(&root, &["hooks"]),
        target.under(&root, &[".minimax-plugin"]),
        root,
    ] {
        let _ = target.host.remove(&dir, false);
    }
    Ok(HookOutcome::Removed)
}

fn minimax_publish_file(
    target: &HookTarget,
    final_path: &Path,
    temp_path: &Path,
    bytes: &[u8],
) -> anyhow::Result<()> {
    if let Some(parent) = final_path.parent() {
        target.host.create_dir(parent, true)?;
    }
    // This path is already tty7's managed staging file. Calling
    // HookTarget::write here would wrap it in config::write_atomic on local
    // hosts and create a second, untracked temporary file.
    target.host.write_file(temp_path, bytes)?;
    match target.host.stat(final_path) {
        Ok(_) => target.host.remove(final_path, false)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(anyhow::anyhow!("read {}: {e}", final_path.display())),
    }
    target.host.rename(temp_path, final_path)?;
    Ok(())
}

fn minimax_reject_plugin_symlinks(target: &HookTarget) -> anyhow::Result<()> {
    for path in target.minimax_plugin_guard_paths() {
        minimax_reject_symlink(target, &path)?;
    }
    Ok(())
}

fn minimax_partial_package_is_ours(
    target: &HookTarget,
    expected_hooks: &str,
    expected_manifest: &str,
) -> anyhow::Result<bool> {
    let root = target.minimax_plugin_root();
    let mut has_hook_marker = false;
    let mut has_unverified_file = false;
    for entry in target.host.read_dir(&root, None)? {
        if entry.is_symlink {
            return Ok(false);
        }
        match entry.name.as_str() {
            ".minimax-plugin" => {
                if !entry.is_dir {
                    return Ok(false);
                }
                for item in target
                    .host
                    .read_dir(&target.under(&root, &[".minimax-plugin"]), None)?
                {
                    if item.is_symlink || item.is_dir || item.name != MINIMAX_PLUGIN_MANIFEST_TEMP {
                        return Ok(false);
                    }
                    match minimax_read_text(target, &target.minimax_plugin_manifest_temp_path()) {
                        Ok(text) if text == expected_manifest => {}
                        Ok(_) => has_unverified_file = true,
                        Err(_) => return Ok(false),
                    }
                }
            }
            "hooks" => {
                if !entry.is_dir {
                    return Ok(false);
                }
                let hooks_dir = target.under(&root, &["hooks"]);
                let entries = target.host.read_dir(&hooks_dir, None)?;
                if entries.len() > 1
                    || entries
                        .iter()
                        .any(|item| item.is_symlink || item.is_dir || item.name != "tty7.json")
                {
                    return Ok(false);
                }
                match minimax_read_text(target, &target.minimax_plugin_hooks_path()) {
                    Ok(text) if minimax_hooks_are_owned(&text, expected_hooks) => {
                        has_hook_marker = true;
                    }
                    Ok(_) => has_unverified_file = true,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(_) => return Ok(false),
                }
            }
            "icon.png" => {
                if entry.is_dir {
                    return Ok(false);
                }
                if minimax_read_bytes(target, &target.minimax_plugin_icon_path())
                    .ok()
                    .as_deref()
                    != Some(MINIMAX_PLUGIN_ICON)
                {
                    has_unverified_file = true;
                }
            }
            MINIMAX_PLUGIN_HOOKS_TEMP => {
                if entry.is_dir {
                    return Ok(false);
                }
                match minimax_read_bytes(target, &target.minimax_plugin_hooks_temp_path()) {
                    Ok(bytes)
                        if std::str::from_utf8(&bytes)
                            .is_ok_and(|text| minimax_hooks_are_owned(text, expected_hooks)) =>
                    {
                        has_hook_marker = true;
                    }
                    // A first write may stop before even the marker is complete.
                    // Accept an exact prefix at this reserved staging path,
                    // but do not use it to claim ownership of other files.
                    Ok(bytes) if expected_hooks.as_bytes().starts_with(&bytes) => {}
                    Ok(_) => has_unverified_file = true,
                    Err(_) => return Ok(false),
                }
            }
            MINIMAX_PLUGIN_ICON_TEMP => {
                if entry.is_dir {
                    return Ok(false);
                }
                has_unverified_file = true;
            }
            _ => return Ok(false),
        }
    }
    Ok(!has_unverified_file || has_hook_marker)
}

fn minimax_cleanup_partial_package(target: &HookTarget, expected_hooks: &str) {
    let hook_owned = minimax_read_text(target, &target.minimax_plugin_hooks_path())
        .ok()
        .is_some_and(|text| minimax_hooks_are_owned(&text, expected_hooks));
    let temp_hook_owned = minimax_read_text(target, &target.minimax_plugin_hooks_temp_path())
        .ok()
        .is_some_and(|text| minimax_hooks_are_owned(&text, expected_hooks));
    if hook_owned || temp_hook_owned {
        let _ = target
            .host
            .remove(&target.minimax_plugin_hooks_path(), false);
        let _ = target
            .host
            .remove(&target.minimax_plugin_icon_path(), false);
    }
    for path in [
        target.minimax_plugin_hooks_temp_path(),
        target.minimax_plugin_icon_temp_path(),
    ] {
        let _ = target.host.remove(&path, false);
    }
    if target
        .host
        .stat(&target.minimax_plugin_manifest_path())
        .is_ok()
    {
        let _ = target
            .host
            .remove(&target.minimax_plugin_manifest_path(), false);
    }
    for dir in [
        target.under(&target.minimax_plugin_root(), &["hooks"]),
        target.under(&target.minimax_plugin_root(), &[".minimax-plugin"]),
        target.minimax_plugin_root(),
    ] {
        let _ = target.host.remove(&dir, false);
    }
}

fn minimax_hooks_contain_ownership_marker(text: &str) -> bool {
    text.contains(MINIMAX_PLUGIN_HOOKS_MARKER) || text.contains(&HookAgent::MiniMaxCode.marker())
}

fn minimax_hooks_are_owned(text: &str, expected_hooks: &str) -> bool {
    text == expected_hooks || minimax_hooks_contain_ownership_marker(text)
}

fn minimax_plugin_manifest() -> anyhow::Result<String> {
    Ok(serde_json::to_string_pretty(&serde_json::json!({
        "schemaVersion": 1,
        "name": MINIMAX_PLUGIN_NAME,
        "displayName": "tty7 Agent Hooks",
        "version": MINIMAX_PLUGIN_VERSION,
        "description": MINIMAX_PLUGIN_DESCRIPTION,
        "author": MINIMAX_PLUGIN_AUTHOR,
        "icon": "icon.png",
        "category": "Other",
        "exampleQueries": [],
        "apps": [],
        "mcpServers": [],
        "skills": [],
        "hooks": ["hooks/tty7.json"],
        "hostBindings": []
    }))?)
}

fn minimax_plugin_hooks(target: &HookTarget) -> anyhow::Result<String> {
    let mut hooks = serde_json::Map::new();
    for (hook_event, tty7_event) in MINIMAX_HOOK_EVENTS {
        hooks.insert(
            (*hook_event).to_string(),
            serde_json::json!([{
                "hooks": [{
                    "type": "command",
                    "command": target.hook_command(HookAgent::MiniMaxCode, tty7_event)
                }]
            }]),
        );
    }
    let mut package = serde_json::Map::new();
    package.insert(
        "_tty7".to_string(),
        serde_json::Value::String(MINIMAX_PLUGIN_HOOKS_MARKER.to_string()),
    );
    package.insert("hooks".to_string(), serde_json::Value::Object(hooks));
    Ok(serde_json::to_string_pretty(&serde_json::Value::Object(
        package,
    ))?)
}

fn minimax_manifest_is_ours(text: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return false;
    };
    value.get("name").and_then(|v| v.as_str()) == Some(MINIMAX_PLUGIN_NAME)
        && value.get("author").and_then(|v| v.as_str()) == Some(MINIMAX_PLUGIN_AUTHOR)
        && value.get("description").and_then(|v| v.as_str()) == Some(MINIMAX_PLUGIN_DESCRIPTION)
}

fn minimax_reject_symlink(target: &HookTarget, path: &Path) -> anyhow::Result<()> {
    match target.host.stat(path) {
        Ok(meta) if meta.is_symlink => Err(anyhow::anyhow!(
            "{} is a symlink; not touching it",
            path.display()
        )),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(anyhow::anyhow!("read {}: {e}", path.display())),
    }
}

fn minimax_read_text(target: &HookTarget, path: &Path) -> io::Result<String> {
    minimax_reject_symlink(target, path).map_err(|e| io::Error::other(e.to_string()))?;
    target.read(path)
}

fn minimax_read_bytes(target: &HookTarget, path: &Path) -> io::Result<Vec<u8>> {
    minimax_reject_symlink(target, path).map_err(|e| io::Error::other(e.to_string()))?;
    target.host.read_file(path, MAX_CONFIG_BYTES)
}

/// Cursor's `hooks.json` events, camel-cased and flat (see
/// [`HookAgent::flat_hook_map`]). The payloads name the session
/// `conversation_id`, which [`build_hook_sequence`] reads as the session id.
///
/// The interactive `cursor-agent` TUI — what runs in a tty7 pane — fires the
/// editor's turn hooks: `beforeSubmitPrompt` opens a turn and `stop` closes
/// it, with a status of `completed`, `aborted` or `error`. `postToolUse` is
/// plain tool activity. Print mode (`cursor-agent -p`) sends neither
/// `beforeSubmitPrompt` nor `stop` (a known gap per Cursor staff, 2026-08),
/// so a `-p` run records its session but shows no status.
///
/// None of these are Cursor's permission hooks, which would block the agent
/// on the empty stdout `tty7 agent-hook` prints.
const CURSOR_HOOK_EVENTS: &[(&str, &str)] = &[
    ("sessionStart", "session-start"),
    ("beforeSubmitPrompt", "prompt-submit"),
    ("postToolUse", "tool-complete"),
    ("stop", "stop"),
    ("sessionEnd", "session-end"),
];

fn hook_map_state(
    target: &HookTarget,
    path: &Path,
    agent: HookAgent,
    events: &[(&str, &str)],
) -> HooksState {
    let Ok(text) = target.read(path) else {
        return HooksState::NotInstalled;
    };
    let Ok(root) = serde_json::from_str::<serde_json::Value>(&text) else {
        return HooksState::NotInstalled;
    };
    let marker = agent.marker();
    let (mut any, mut complete) = (false, true);
    // A file without the version Cursor demands is ignored by it, hooks and
    // all. Only a missing one counts: install never overwrites the user's.
    if agent.hook_map_version().is_some() && root.get("version").is_none() {
        complete = false;
    }
    for (hook_event, tty7_event) in events {
        let ours = root
            .get("hooks")
            .and_then(|h| h.get(hook_event))
            .and_then(|e| e.as_array())
            .and_then(|list| list.iter().find_map(|m| marker_command(m, &marker)));
        match ours {
            Some(cmd) => {
                any = true;
                if cmd != target.hook_command(agent, tty7_event) {
                    complete = false;
                }
            }
            None => complete = false,
        }
    }
    match (any, complete) {
        (false, _) => HooksState::NotInstalled,
        (true, true) => HooksState::Installed,
        (true, false) => HooksState::Outdated,
    }
}

fn hook_map_install(
    target: &HookTarget,
    path: &Path,
    agent: HookAgent,
    events: &[(&str, &str)],
) -> anyhow::Result<()> {
    let mut root: serde_json::Value = match target.read(path) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| {
            anyhow::anyhow!(
                "{} is not valid JSON ({e}); not touching it",
                path.display()
            )
        })?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => serde_json::json!({}),
        Err(e) => return Err(anyhow::anyhow!("read {}: {e}", path.display())),
    };
    if !root.is_object() {
        return Err(anyhow::anyhow!(
            "{} is not a JSON object; not touching it",
            path.display()
        ));
    }

    if let Some(version) = agent.hook_map_version() {
        root.as_object_mut()
            .unwrap()
            .entry("version")
            .or_insert_with(|| serde_json::json!(version));
    }

    let hooks = root
        .as_object_mut()
        .unwrap()
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}));
    if !hooks.is_object() {
        return Err(anyhow::anyhow!(
            "\"hooks\" in {} is not an object; not touching it",
            path.display()
        ));
    }

    let marker = agent.marker();
    for (hook_event, tty7_event) in events {
        let command = target.hook_command(agent, tty7_event);
        let entries = hooks
            .as_object_mut()
            .unwrap()
            .entry(*hook_event)
            .or_insert_with(|| serde_json::json!([]));
        let Some(list) = entries.as_array_mut() else {
            continue;
        };
        list.retain(|matcher| marker_command(matcher, &marker).is_none());
        if agent.flat_hook_map() {
            list.push(serde_json::json!({ "command": command }));
        } else {
            list.push(serde_json::json!({
                "hooks": [{ "type": "command", "command": command }]
            }));
        }
    }

    target.write(path, serde_json::to_string_pretty(&root)?.as_bytes())
}

fn hook_map_uninstall(
    target: &HookTarget,
    path: &Path,
    agent: HookAgent,
) -> anyhow::Result<HookOutcome> {
    let text = match target.read(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Ok(HookOutcome::NothingInstalled);
        }
        Err(e) => return Err(anyhow::anyhow!("read {}: {e}", path.display())),
    };
    let mut root: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
        anyhow::anyhow!(
            "{} is not valid JSON ({e}); not touching it",
            path.display()
        )
    })?;

    let marker = agent.marker();
    let mut removed = 0;
    if let Some(hooks) = root.get_mut("hooks").and_then(|h| h.as_object_mut()) {
        for entries in hooks.values_mut() {
            if let Some(list) = entries.as_array_mut() {
                let before = list.len();
                list.retain(|matcher| marker_command(matcher, &marker).is_none());
                removed += before - list.len();
            }
        }
        hooks.retain(|_, entries| entries.as_array().is_none_or(|list| !list.is_empty()));
    }
    if removed == 0 {
        return Ok(HookOutcome::NoTty7Hooks);
    }
    target.write(path, serde_json::to_string_pretty(&root)?.as_bytes())?;
    Ok(HookOutcome::Removed)
}

/// The tty7 command an entry in a hook map carries, if it is one of ours.
///
/// Two shapes reach here. Claude and its imitators nest it at
/// `entry.hooks[].command`; Crush lifts `command` to the entry itself, the
/// same level as its `matcher`. Both are one list under `hooks.<Event>`, so
/// the state reader, the installer and the uninstaller all stay shared.
fn marker_command<'a>(entry: &'a serde_json::Value, marker: &str) -> Option<&'a str> {
    if let Some(command) = entry
        .get("command")
        .and_then(|c| c.as_str())
        .filter(|c| c.contains(marker))
    {
        return Some(command);
    }
    entry
        .get("hooks")
        .and_then(|h| h.as_array())?
        .iter()
        .find_map(|h| {
            h.get("command")
                .and_then(|c| c.as_str())
                .filter(|c| c.contains(marker))
        })
}

fn toml_hooks_state(
    target: &HookTarget,
    path: &Path,
    agent: HookAgent,
    events: &[(&str, &str)],
) -> HooksState {
    let Ok(text) = target.read(path) else {
        return HooksState::NotInstalled;
    };
    let Ok(doc) = text.parse::<toml_edit::DocumentMut>() else {
        return HooksState::NotInstalled;
    };
    let marker = agent.marker();
    let marked: Vec<&toml_edit::Table> = doc
        .get("hooks")
        .and_then(|h| h.as_array_of_tables())
        .into_iter()
        .flatten()
        .filter(|entry| toml_command_is_marked(entry, &marker))
        .collect();
    if marked.is_empty() {
        return HooksState::NotInstalled;
    }
    // Every marked entry counts towards the total, `event` or no `event`: a
    // hand-edit that drops the key leaves an entry that is ours and is broken,
    // which is exactly what Outdated means. Reporting NotInstalled instead
    // would hide it from `refresh_hooks`, which only ever revisits Outdated.
    let complete = marked.len() == events.len()
        && events.iter().all(|(hook_event, tty7_event)| {
            let command = target.hook_command(agent, tty7_event);
            marked.iter().any(|entry| {
                entry.get("event").and_then(|e| e.as_str()) == Some(*hook_event)
                    && entry.get("command").and_then(|c| c.as_str()) == Some(command.as_str())
            })
        });
    if complete {
        HooksState::Installed
    } else {
        HooksState::Outdated
    }
}

fn toml_hooks_install(
    target: &HookTarget,
    path: &Path,
    agent: HookAgent,
    events: &[(&str, &str)],
) -> anyhow::Result<()> {
    let mut doc: toml_edit::DocumentMut = match target.read(path) {
        Ok(text) => text.parse().map_err(|e| {
            anyhow::anyhow!(
                "{} is not valid TOML ({e}); not touching it",
                path.display()
            )
        })?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => toml_edit::DocumentMut::new(),
        Err(e) => return Err(anyhow::anyhow!("read {}: {e}", path.display())),
    };

    // `hooks = []` and no `hooks` key at all say the same thing, but toml_edit
    // keeps an empty inline array and an array of tables apart. Promote the
    // one to the other rather than refusing over a difference that carries no
    // configuration and that the user cannot see.
    if doc
        .get("hooks")
        .and_then(|h| h.as_array())
        .is_some_and(|a| a.is_empty())
    {
        doc.remove("hooks");
    }

    let hooks = doc.entry("hooks").or_insert(toml_edit::Item::ArrayOfTables(
        toml_edit::ArrayOfTables::new(),
    ));
    let Some(list) = hooks.as_array_of_tables_mut() else {
        return Err(anyhow::anyhow!(
            "\"hooks\" in {} is not an array of tables; not touching it",
            path.display()
        ));
    };

    let marker = agent.marker();
    list.retain(|entry| !toml_command_is_marked(entry, &marker));
    for (hook_event, tty7_event) in events {
        let mut entry = toml_edit::Table::new();
        entry["event"] = toml_edit::value(*hook_event);
        entry["command"] = toml_edit::value(target.hook_command(agent, tty7_event));
        list.push(entry);
    }

    target.write(path, doc.to_string().as_bytes())
}

fn toml_hooks_uninstall(
    target: &HookTarget,
    path: &Path,
    agent: HookAgent,
) -> anyhow::Result<HookOutcome> {
    let text = match target.read(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Ok(HookOutcome::NothingInstalled);
        }
        Err(e) => return Err(anyhow::anyhow!("read {}: {e}", path.display())),
    };
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e| {
        anyhow::anyhow!(
            "{} is not valid TOML ({e}); not touching it",
            path.display()
        )
    })?;

    let marker = agent.marker();
    let mut removed = 0;
    if let Some(list) = doc
        .get_mut("hooks")
        .and_then(|h| h.as_array_of_tables_mut())
    {
        let before = list.len();
        list.retain(|entry| !toml_command_is_marked(entry, &marker));
        removed = before - list.len();
        if list.is_empty() {
            doc.remove("hooks");
        }
    }
    if removed == 0 {
        return Ok(HookOutcome::NoTty7Hooks);
    }
    target.write(path, doc.to_string().as_bytes())?;
    Ok(HookOutcome::Removed)
}

fn toml_command_is_marked(entry: &toml_edit::Table, marker: &str) -> bool {
    entry
        .get("command")
        .and_then(|c| c.as_str())
        .is_some_and(|c| c.contains(marker))
}

/// Returns the bare file name of `exe` when resolving that name from PATH
/// yields the same binary. Returns `None` when the name does not resolve, or
/// when an earlier PATH entry contains a different file with the same name
/// (a bare-name command would then invoke the wrong binary).
#[cfg(windows)]
fn path_resolvable_name(exe: &Path) -> Option<String> {
    let name = exe.file_name()?.to_str()?;
    let path_var = std::env::var_os("PATH")?;
    let exe_canonical = std::fs::canonicalize(exe).ok()?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        let Ok(candidate_canonical) = std::fs::canonicalize(&candidate) else {
            continue;
        };
        if same_windows_path(&candidate_canonical, &exe_canonical) {
            return Some(name.to_string());
        }
        return None;
    }
    None
}

#[cfg(windows)]
fn same_windows_path(a: &Path, b: &Path) -> bool {
    a.to_string_lossy()
        .eq_ignore_ascii_case(&b.to_string_lossy())
}

fn enable_codex_hooks_feature() -> Result<(), String> {
    let candidates = [
        PathBuf::from("/opt/homebrew/bin/codex"),
        PathBuf::from("/usr/local/bin/codex"),
    ]
    .into_iter()
    .chain(home_dir().map(|h| h.join(".local/bin/codex")))
    .find(|p| p.exists());
    let program = candidates.unwrap_or_else(|| PathBuf::from("codex"));
    let mut cmd = std::process::Command::new(&program);
    cmd.args(["features", "enable", "hooks"]);
    match crate::core::proc::hide_console(&mut cmd).output() {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => Err(format!(
            "codex exited with {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )),
        Err(e) => Err(format!("{}: {e}", program.display())),
    }
}

fn owned_file_content(target: &HookTarget, agent: HookAgent) -> Option<String> {
    match agent {
        HookAgent::Copilot => copilot_hooks_json(target),
        HookAgent::OpenCode => opencode_plugin_js(target),
        HookAgent::Pi | HookAgent::OhMyPi => pi_extension_ts(target, agent),
        HookAgent::Grok => grok_hooks_json(target),
        HookAgent::Goose => goose_hooks_json(target),
        HookAgent::Claude
        | HookAgent::Codex
        | HookAgent::TraeCode
        | HookAgent::Gemini
        | HookAgent::Droid
        | HookAgent::Qwen
        | HookAgent::QoderCLI
        | HookAgent::Crush
        | HookAgent::Kimi
        | HookAgent::CommandCode
        | HookAgent::MiniMaxCode => None,
        HookAgent::CodeBuddy | HookAgent::Cursor => None,
    }
}

fn owned_file_state(target: &HookTarget, path: &Path, expected: &str, marker: &str) -> HooksState {
    let Ok(contents) = target.read(path) else {
        return HooksState::NotInstalled;
    };
    if contents == expected {
        HooksState::Installed
    } else if contents.contains(marker) {
        HooksState::Outdated
    } else {
        HooksState::NotInstalled
    }
}

fn owned_file_install(
    target: &HookTarget,
    path: &Path,
    content: &str,
    marker: &str,
) -> anyhow::Result<()> {
    if let Ok(existing) = target.read(path)
        && !existing.contains(marker)
    {
        return Err(anyhow::anyhow!(
            "{} exists but wasn't written by tty7; not touching it",
            path.display()
        ));
    }
    target.write(path, content.as_bytes())
}

fn owned_file_uninstall(
    target: &HookTarget,
    path: &Path,
    marker: &str,
) -> anyhow::Result<HookOutcome> {
    let contents = match target.read(path) {
        Ok(c) => c,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Ok(HookOutcome::NothingInstalled);
        }
        Err(e) => return Err(anyhow::anyhow!("read {}: {e}", path.display())),
    };
    if !contents.contains(marker) {
        return Err(anyhow::anyhow!(
            "{} wasn't written by tty7; not touching it",
            path.display()
        ));
    }
    target.host.remove(path, false)?;
    // Take the directories tty7 generated with it, innermost first, stopping at
    // the one named after tty7. `remove` is not recursive, so a directory still
    // holding someone else's file simply survives the attempt. Goose nests one
    // level deeper than the rest (`.../tty7/hooks/hooks.json`), which is why
    // this walks rather than checking a single parent.
    let mut dir = path.parent();
    while let Some(d) = dir {
        if d.file_name().is_some_and(|n| n == "tty7") {
            let _ = target.host.remove(d, false);
            break;
        }
        if !d
            .parent()
            .and_then(|p| p.file_name())
            .is_some_and(|n| n == "tty7")
        {
            break;
        }
        let _ = target.host.remove(d, false);
        dir = d.parent();
    }
    Ok(HookOutcome::Removed)
}

fn copilot_hooks_json(target: &HookTarget) -> Option<String> {
    let hook = |event: &str, timeout: u32| {
        serde_json::json!([{
            "type": "command",
            "bash": target.hook_command(HookAgent::Copilot, event),
            "timeoutSec": timeout,
        }])
    };
    let root = serde_json::json!({
        "version": 1,
        "hooks": {
            "sessionStart": hook("session-start", 5),
            "userPromptSubmitted": hook("prompt-submit", 5),
            "agentStop": hook("stop", 10),
            "sessionEnd": hook("session-end", 5),
            "notification": hook("notification", 5),
        }
    });
    serde_json::to_string_pretty(&root).ok()
}

fn grok_hooks_json(target: &HookTarget) -> Option<String> {
    let mut hooks = serde_json::Map::new();
    for (event, sentinel, matcher) in GROK_HOOK_EVENTS {
        let mut group = serde_json::json!({
            "hooks": [{
                "type": "command",
                "command": target.hook_command(HookAgent::Grok, sentinel),
                "timeout": GROK_HOOK_TIMEOUT_SECS,
            }]
        });
        if let Some(matcher) = matcher {
            group["matcher"] = serde_json::Value::String((*matcher).to_string());
        }
        hooks.insert((*event).to_string(), serde_json::json!([group]));
    }
    serde_json::to_string_pretty(&serde_json::json!({ "hooks": hooks })).ok()
}

/// Goose has no permission hook — `PreToolUse` fires on every call, approved or
/// not, so there is nothing here that could report a blocked turn. The four
/// events it does have still carry the pane from idle to working to done.
const GOOSE_HOOK_EVENTS: &[(&str, &str)] = &[
    ("SessionStart", "session-start"),
    ("UserPromptSubmit", "prompt-submit"),
    ("PostToolUse", "tool-complete"),
    ("Stop", "stop"),
    ("SessionEnd", "session-end"),
];

fn goose_hooks_json(target: &HookTarget) -> Option<String> {
    let mut hooks = serde_json::Map::new();
    for (event, sentinel) in GOOSE_HOOK_EVENTS {
        hooks.insert(
            (*event).to_string(),
            serde_json::json!([{
                "hooks": [{
                    "type": "command",
                    "command": target.hook_command(HookAgent::Goose, sentinel),
                }]
            }]),
        );
    }
    serde_json::to_string_pretty(&serde_json::json!({ "hooks": hooks })).ok()
}

fn opencode_plugin_js(target: &HookTarget) -> Option<String> {
    let prefix = serde_json::to_string(&format!(
        "{} ",
        target.hook_command(HookAgent::OpenCode, "").trim_end()
    ))
    .ok()?;
    Some(format!(
        r#"// tty7 agent-hook opencode bridge — generated by tty7, do not edit.
// Bridges OpenCode plugin events onto `tty7 agent-hook opencode <event>`,
// which is inert outside tty7 (gated on the TTY7 env var).
export const Tty7Presence = async ({{ $ }}) => {{
  if (!process.env["TTY7"]) return {{}}
  const cmd = {prefix}
  let sessionId = ""
  let announced = ""
  // A subagent runs in a child session on the *same* event stream, and its
  // status events are indistinguishable from the pane's own — only
  // `session.created`/`session.updated` name a parent. Remember the children,
  // so a task tool cannot hand the pane the wrong session to resume, nor call
  // the pane done when only the subagent is.
  const children = new Set()
  const emit = (event) => {{
    // Every event carries the session id (`properties.sessionID`), so a pane
    // that restarts can resume the same session with `opencode --session`.
    const payload = sessionId ? new Response(JSON.stringify({{ session_id: sessionId }})) : undefined
    const proc = payload ? $`sh -c ${{cmd + event}} < ${{payload}}` : $`sh -c ${{cmd + event}}`
    return proc.quiet().nothrow()
  }}
  // The session id is only known once opencode creates the session (the
  // `session.created` event); the session-start report rides on the first
  // event that names one, so a restored pane can reattach to it.
  const capture = async (id) => {{
    if (id && !children.has(id)) sessionId = id
    if (sessionId && sessionId !== announced) {{
      announced = sessionId
      await emit("session-start")
    }}
  }}
  const ACTION = {{
    "session.status.busy": "prompt-submit",
    "session.status.idle": "stop",
    "session.idle": "stop",
    "permission.replied": "prompt-submit",
  }}

  return {{
    dispose: async () => {{
      await emit("session-end")
    }},
    "tool.execute.before": async (input) => {{
      await capture(input?.sessionID)
      await emit("prompt-submit")
    }},
    "permission.ask": async (input) => {{
      await capture(input?.sessionID)
      await emit("permission-request")
    }},
    event: async ({{ event }}) => {{
      const properties = event.properties ?? {{}}
      const info = properties.info
      if (info?.id && info.parentID) children.add(info.id)
      if (properties.sessionID && children.has(properties.sessionID)) return
      await capture(properties.sessionID)
      const key = event.type === "session.status" ? `session.status.${{properties.status?.type}}` : event.type
      const action = ACTION[key]
      if (action) await emit(action)
    }},
  }}
}}
"#
    ))
}

/// The Pi extension bridge, shared with Oh My Pi.
///
/// Oh My Pi is a fork of Pi and kept the extension contract intact — same
/// default-exported factory, same four lifecycle events, same
/// `ctx.sessionManager.getSessionId()`. Only the package it is imported from
/// and the slug the emitter is called with differ, so one template serves both
/// rather than two copies drifting apart.
fn pi_extension_ts(target: &HookTarget, agent: HookAgent) -> Option<String> {
    let (slug, package) = match agent {
        HookAgent::Pi => ("pi", "@mariozechner/pi-coding-agent"),
        HookAgent::OhMyPi => ("omp", "@oh-my-pi/pi-coding-agent"),
        _ => return None,
    };
    let exe = serde_json::to_string(&target.exe.display().to_string()).ok()?;
    Some(format!(
        r#"/* tty7 agent-hook {slug} bridge — generated by tty7, do not edit. */
import type {{ ExtensionAPI }} from "{package}";
import {{ spawnSync }} from "node:child_process";

const EXE = {exe};

/** The slice of Pi's handler context we read — structural, so this bridge does
 *  not depend on the context type staying exported. */
type SessionCtx = {{ sessionManager?: {{ getSessionId?(): string | undefined }} }};

function emit(event: string, ctx?: SessionCtx): void {{
  try {{
    let payload = "";
    try {{
      const id = ctx?.sessionManager?.getSessionId?.();
      if (id) payload = JSON.stringify({{ session_id: id }});
    }} catch {{}}
    const args = ["agent-hook", "{slug}", event];
    // Nothing to send → leave stdin closed rather than handing the emitter a
    // pipe it has to read to EOF.
    if (payload) {{
      spawnSync(EXE, args, {{ input: payload, stdio: ["pipe", "ignore", "ignore"] }});
    }} else {{
      spawnSync(EXE, args, {{ stdio: ["ignore", "ignore", "ignore"] }});
    }}
  }} catch {{}}
}}

export default function (pi: ExtensionAPI) {{
  if (!process.env["TTY7"]) return;
  // Extension load = the agent is running in this pane. No context here yet,
  // so the id rides on session_start instead.
  emit("session-start");
  // What the pane showed before a UI prompt put it on "waiting", so closing
  // the prompt can put it back.
  let turn = "session-start";
  pi.on("agent_start", (_event, ctx) => emit((turn = "prompt-submit"), ctx));
  pi.on("agent_end", (_event, ctx) => emit((turn = "stop"), ctx));
  pi.on("session_shutdown", (_event, ctx) => emit("session-end", ctx));
  // Last, and guarded: the three above already worked, so a Pi build that
  // rejects this event name must not take them — or the whole extension —
  // down with it.
  try {{
    pi.on("session_start", (_event, ctx) => emit("session-start", ctx));
  }} catch {{}}
  // Pi-only: Oh My Pi may not expose the UI-prompt hooks, so each one is
  // guarded on its own — a fork that rejects the event name must not take the
  // rest of the bridge down with it. Without these the pane never reports
  // "waiting for you" while a dialog is open, and the event vocabulary already
  // carries question-asked / permission-request for exactly that.
  try {{
    pi.on("ui_prompt_start", (event, ctx) => {{
      emit(event.kind === "confirm" ? "permission-request" : "question-asked", ctx);
    }});
  }} catch {{}}
  // The dialog closed: restore what it interrupted. Not a blanket
  // prompt-submit — Pi also prompts while idle (/model, a command's select),
  // and that would leave a finished pane reading "working" with no turn to
  // ever end it.
  try {{
    pi.on("ui_prompt_end", (_event, ctx) => emit(turn, ctx));
  }} catch {{}}
}}
"#
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exhaustive match keeps every detected agent mapped; this keeps the
    /// other direction honest, so a hooked agent cannot become unreachable
    /// from detection and silently stop being diagnosed.
    #[test]
    fn every_hooked_agent_is_reachable_from_detection() {
        for hooked in HookAgent::ALL {
            assert!(
                CLIAgent::ALL
                    .into_iter()
                    .any(|detected| HookAgent::of_detected(detected) == Some(hooked)),
                "{hooked:?} has hooks but no detected agent maps to it"
            );
        }
    }

    #[test]
    fn every_tty7_daemon_host_takes_the_console_fast_path() {
        for name in [
            "tty7-app.exe",
            "tty7-server.exe",
            "tty7.exe",
            "/Applications/tty7.app/Contents/MacOS/tty7-server",
            "/home/me/.local/share/tty7/bin/tty7-server-c3p4",
        ] {
            assert!(is_tty7_host_exe(name), "{name} hosts tty7 shells");
        }
        for name in [
            "explorer.exe",
            "cmd.exe",
            "wt.exe",
            "tty7-server-cxpy",
            "tty7-server-c3p4.bak",
        ] {
            assert!(!is_tty7_host_exe(name), "{name} is not a tty7 host process");
        }
    }

    #[test]
    fn hook_ancestor_preserves_spaces_in_executable_paths() {
        assert_eq!(
            parse_hook_ancestor("  42 /Applications/My Apps/tty7.app/Contents/MacOS/tty7-app\n"),
            Some((42, "/Applications/My Apps/tty7.app/Contents/MacOS/tty7-app"))
        );
        assert_eq!(
            parse_hook_ancestor("7 /opt/Node Runtime/bin/node"),
            Some((7, "/opt/Node Runtime/bin/node"))
        );
        for invalid in ["", "42", "42   ", "bad /bin/node"] {
            assert_eq!(parse_hook_ancestor(invalid), None);
        }
    }

    #[test]
    fn hook_sequence_round_trips_through_the_daemon_parser() {
        use crate::core::cli_agent::{AgentEventKind, CLIAgent, parse_agent_event};

        let seq = build_hook_sequence(
            "claude",
            "notification",
            r#"{"session_id":"abc-123","message":"Claude needs your permission","cwd":"/w"}"#,
        );
        let payload = &seq[2..seq.len() - 1];
        let ev = parse_agent_event(payload).expect("daemon parses the emitted event");
        assert_eq!(ev.agent, Some(CLIAgent::Claude));
        assert_eq!(ev.kind, AgentEventKind::Notification);
        assert_eq!(ev.session_id.as_deref(), Some("abc-123"));
        assert!(ev.message.as_deref().unwrap().contains("permission"));
        assert_eq!(ev.cwd.as_deref(), Some(std::path::Path::new("/w")));

        let seq = build_hook_sequence("claude", "stop", "not json at all");
        let ev = parse_agent_event(&seq[2..seq.len() - 1]).expect("bare event still parses");
        assert_eq!(ev.kind, AgentEventKind::Stop);
        assert_eq!(ev.session_id, None);

        let seq = build_hook_sequence(
            "grok",
            "session-start",
            r#"{"hookEventName":"session_start","sessionId":"g-42","cwd":"/w"}"#,
        );
        let ev = parse_agent_event(&seq[2..seq.len() - 1]).expect("daemon parses the grok event");
        assert_eq!(ev.agent, Some(CLIAgent::Grok));
        assert_eq!(ev.session_id.as_deref(), Some("g-42"));
        assert_eq!(ev.cwd.as_deref(), Some(std::path::Path::new("/w")));
    }

    /// Parses a built sequence back the way the terminal's scanner does.
    fn round_trip(
        agent: &str,
        event: &str,
        stdin_json: &str,
    ) -> crate::core::cli_agent::AgentEvent {
        let seq = build_hook_sequence(agent, event, stdin_json);
        crate::core::cli_agent::parse_agent_event(&seq[2..seq.len() - 1]).expect("parses")
    }

    #[test]
    fn a_submitted_prompt_rides_back_as_the_turns_label() {
        let ev = round_trip(
            "claude",
            "prompt-submit",
            r#"{"prompt":"restore the outline","session_id":"s-1"}"#,
        );
        assert_eq!(ev.prompt.as_deref(), Some("restore the outline"));
        assert_eq!(
            ev.message, None,
            "a prompt is not a message; the turn starts with nothing said back"
        );
    }

    #[test]
    fn a_prompt_is_cut_to_its_first_line() {
        let ev = round_trip(
            "claude",
            "prompt-submit",
            r#"{"prompt":"\n\n  what did we decide  \nand then some more\nand more"}"#,
        );
        assert_eq!(
            ev.prompt.as_deref(),
            Some("what did we decide"),
            "later lines wrapped when they were drawn and would only fail the search"
        );
    }

    #[test]
    fn a_pasted_file_cannot_cost_the_whole_event() {
        let prompt = "x".repeat(64 * 1024);
        let ev = round_trip(
            "claude",
            "prompt-submit",
            &serde_json::json!({ "prompt": prompt }).to_string(),
        );
        assert_eq!(
            ev.prompt.map(|p| p.chars().count()),
            Some(PROMPT_LABEL_MAX),
            "the tokenizer abandons an oversized payload rather than truncating it"
        );
    }

    #[test]
    fn a_prompt_of_wide_characters_is_cut_on_a_character_boundary() {
        let prompt = "把大纲恢复一下".repeat(100);
        let ev = round_trip(
            "claude",
            "prompt-submit",
            &serde_json::json!({ "prompt": prompt }).to_string(),
        );
        assert_eq!(ev.prompt.map(|p| p.chars().count()), Some(PROMPT_LABEL_MAX));
    }

    #[test]
    fn an_agent_that_reports_no_prompt_carries_none() {
        assert_eq!(round_trip("codex", "stop", "{}").prompt, None);
        assert_eq!(
            round_trip("claude", "prompt-submit", r#"{"prompt":"   "}"#).prompt,
            None,
            "whitespace is not a label"
        );
    }

    #[test]
    fn grok_run_hooks_are_relabeled_to_grok() {
        assert_eq!(effective_agent("claude", true), "grok");
        assert_eq!(effective_agent("grok", true), "grok");
        assert_eq!(effective_agent("claude", false), "claude");
        assert_eq!(effective_agent("grok", false), "grok");
    }

    #[test]
    fn minimax_hook_runner_requires_tty7_ownership_without_the_environment_marker() {
        assert!(hook_runner_enabled("minimax-code", true, false));
        assert!(hook_runner_enabled("minimax-code", false, true));
        assert!(!hook_runner_enabled("minimax-code", false, false));
        assert!(hook_runner_enabled("claude", true, false));
        assert!(!hook_runner_enabled("claude", false, true));
    }

    #[test]
    fn every_installed_event_parses_as_a_sentinel_kind() {
        use crate::core::cli_agent::parse_agent_event;

        let mut events: Vec<&str> = CLAUDE_HOOK_EVENTS
            .iter()
            .chain(CODEX_HOOK_EVENTS)
            .chain(TRAE_CODE_HOOK_EVENTS)
            .chain(GEMINI_HOOK_EVENTS)
            .chain(DROID_HOOK_EVENTS)
            .chain(QODER_HOOK_EVENTS)
            .chain(QWEN_HOOK_EVENTS)
            .chain(GOOSE_HOOK_EVENTS)
            .chain(KIMI_HOOK_EVENTS)
            .chain(CRUSH_HOOK_EVENTS)
            .chain(COMMANDCODE_HOOK_EVENTS)
            .chain(MINIMAX_HOOK_EVENTS)
            .chain(CODEBUDDY_HOOK_EVENTS)
            .chain(CURSOR_HOOK_EVENTS)
            .map(|(_, e)| *e)
            .chain(GROK_HOOK_EVENTS.iter().map(|(_, e, _)| *e))
            .collect();
        events.extend([
            "prompt-submit",
            "permission-request",
            "stop",
            "session-end",
            "session-start",
        ]);
        for event in events {
            let seq = build_hook_sequence("codex", event, "{}");
            let ev = parse_agent_event(&seq[2..seq.len() - 1])
                .unwrap_or_else(|| panic!("event {event:?} must parse"));
            let kind_json = serde_json::to_value(ev.kind).unwrap();
            assert_eq!(kind_json, serde_json::Value::String(event.to_string()));
        }
    }

    #[test]
    fn command_code_uses_pre_tool_as_its_turn_start() {
        assert_eq!(
            COMMANDCODE_HOOK_EVENTS,
            [
                ("SessionStart", "session-start"),
                ("PreToolUse", "prompt-submit"),
                ("PostToolUse", "tool-complete"),
                ("Stop", "stop"),
            ]
        );
    }

    #[test]
    fn the_new_hook_agents_target_the_paths_their_clis_read() {
        let host = FakeRemote::shared();
        let t = HookTarget::remote(&*host, PathBuf::from("/home/me"));

        for (agent, want) in [
            (HookAgent::Gemini, "/home/me/.gemini/settings.json"),
            (HookAgent::Droid, "/home/me/.factory/settings.json"),
            (HookAgent::Qwen, "/home/me/.qwen/settings.json"),
            (HookAgent::TraeCode, "/home/me/.trae/cli/hooks.json"),
            (
                HookAgent::Goose,
                "/home/me/.agents/plugins/tty7/hooks/hooks.json",
            ),
            (HookAgent::Kimi, "/home/me/.kimi-code/config.toml"),
            (HookAgent::QoderCLI, "/home/me/.qoder/settings.json"),
            (HookAgent::Crush, "/home/me/.config/crush/crush.json"),
            (
                HookAgent::CommandCode,
                "/home/me/.commandcode/settings.json",
            ),
            (
                HookAgent::MiniMaxCode,
                "/home/me/.minimax/plugins/tty7-agent-hooks/.minimax-plugin/plugin.json",
            ),
            (HookAgent::CodeBuddy, "/home/me/.codebuddy/settings.json"),
            (HookAgent::Cursor, "/home/me/.cursor/hooks.json"),
        ] {
            assert_eq!(
                agent.target_path(&t),
                PathBuf::from(want),
                "{} writes somewhere its CLI does not read",
                agent.slug()
            );
        }

        let dir = std::env::temp_dir().join(format!("tty7-new-hooks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let real = HookTarget::remote(&*host, dir.clone());

        for agent in [
            HookAgent::Gemini,
            HookAgent::Droid,
            HookAgent::Qwen,
            HookAgent::Goose,
            HookAgent::Kimi,
            HookAgent::QoderCLI,
            HookAgent::Crush,
            HookAgent::CommandCode,
            HookAgent::CodeBuddy,
            HookAgent::Cursor,
        ] {
            assert_eq!(hooks_state(&real, agent), HooksState::NotInstalled);
            install_hooks(&real, agent).unwrap_or_else(|e| panic!("{}: {e}", agent.slug()));
            assert_eq!(
                hooks_state(&real, agent),
                HooksState::Installed,
                "{} does not read back what it wrote",
                agent.slug()
            );
            let written = std::fs::read_to_string(agent.target_path(&real)).unwrap();
            assert!(
                written.contains(&format!("agent-hook {}", agent.slug())),
                "{} wrote a config without its own emitter",
                agent.slug()
            );
            uninstall_hooks(&real, agent).unwrap_or_else(|e| panic!("{}: {e}", agent.slug()));
            assert_eq!(hooks_state(&real, agent), HooksState::NotInstalled);
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn minimax_data_dir_prefers_local_overrides_and_never_redirects_remote_hooks() {
        let home = Path::new("/home/me");
        assert_eq!(
            minimax_data_dir_for(
                home,
                true,
                Some(std::ffi::OsStr::new("/custom/minimax")),
                Some(std::ffi::OsStr::new("/legacy/mavis")),
            ),
            PathBuf::from("/custom/minimax")
        );
        assert_eq!(
            minimax_data_dir_for(
                home,
                true,
                None,
                Some(std::ffi::OsStr::new("/legacy/mavis")),
            ),
            PathBuf::from("/legacy/mavis")
        );
        assert_eq!(
            minimax_data_dir_for(home, true, None, None),
            PathBuf::from("/home/me/.minimax")
        );
        assert_eq!(
            minimax_data_dir_for(
                home,
                false,
                Some(std::ffi::OsStr::new("/local/override")),
                Some(std::ffi::OsStr::new("/local/legacy")),
            ),
            PathBuf::from("/home/me/.minimax")
        );
    }

    #[test]
    fn minimax_installs_and_removes_a_native_plugin_package() {
        let dir = std::env::temp_dir().join(format!("tty7-minimax-plugin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let host = FakeRemote::shared();
        let target = HookTarget::remote(&*host, dir.clone());

        assert_eq!(
            hooks_state(&target, HookAgent::MiniMaxCode),
            HooksState::NotInstalled
        );
        install_hooks(&target, HookAgent::MiniMaxCode).expect("MiniMax install succeeds");
        assert_eq!(
            hooks_state(&target, HookAgent::MiniMaxCode),
            HooksState::Installed
        );

        let manifest: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(target.minimax_plugin_manifest_path()).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest["schemaVersion"], 1);
        assert_eq!(manifest["name"], MINIMAX_PLUGIN_NAME);
        assert_eq!(manifest["hooks"], serde_json::json!(["hooks/tty7.json"]));
        assert_eq!(manifest["icon"], "icon.png");

        let hooks: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(target.minimax_plugin_hooks_path()).unwrap(),
        )
        .unwrap();
        for (hook_event, tty7_event) in MINIMAX_HOOK_EVENTS {
            let command = hooks["hooks"][*hook_event][0]["hooks"][0]["command"]
                .as_str()
                .unwrap();
            assert_eq!(
                command,
                target.hook_command(HookAgent::MiniMaxCode, tty7_event)
            );
        }
        assert_eq!(
            std::fs::read(target.minimax_plugin_icon_path()).unwrap(),
            MINIMAX_PLUGIN_ICON
        );

        install_hooks(&target, HookAgent::MiniMaxCode).expect("re-install succeeds");
        assert_eq!(
            uninstall_hooks(&target, HookAgent::MiniMaxCode).unwrap(),
            HookOutcome::Removed
        );
        assert_eq!(
            hooks_state(&target, HookAgent::MiniMaxCode),
            HooksState::NotInstalled
        );
        assert!(!target.minimax_plugin_root().exists());
        assert_eq!(
            uninstall_hooks(&target, HookAgent::MiniMaxCode).unwrap(),
            HookOutcome::NothingInstalled
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn minimax_reinstalls_from_an_owned_partial_package() {
        let dir = std::env::temp_dir().join(format!(
            "tty7-minimax-plugin-partial-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let host = FakeRemote::shared();
        let target = HookTarget::remote(&*host, dir.clone());
        let hooks_path = target.minimax_plugin_hooks_path();
        std::fs::create_dir_all(hooks_path.parent().unwrap()).unwrap();
        std::fs::write(&hooks_path, minimax_plugin_hooks(&target).unwrap()).unwrap();

        install_hooks(&target, HookAgent::MiniMaxCode)
            .expect("an owned partial package must be recoverable");
        assert_eq!(
            hooks_state(&target, HookAgent::MiniMaxCode),
            HooksState::Installed
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn minimax_recovers_from_truncated_remote_package_files() {
        let dir = std::env::temp_dir().join(format!(
            "tty7-minimax-plugin-truncated-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let host = FakeRemote::shared();
        let target = HookTarget::remote(&*host, dir.clone());
        let hooks_path = target.minimax_plugin_hooks_path();
        std::fs::create_dir_all(hooks_path.parent().unwrap()).unwrap();
        std::fs::write(
            &hooks_path,
            format!(r#"{{"_tty7":"{MINIMAX_PLUGIN_HOOKS_MARKER}","hooks":{{"#),
        )
        .unwrap();
        std::fs::write(target.minimax_plugin_icon_path(), b"truncated").unwrap();

        install_hooks(&target, HookAgent::MiniMaxCode)
            .expect("truncated owned files must not block a reinstall");
        assert_eq!(
            hooks_state(&target, HookAgent::MiniMaxCode),
            HooksState::Installed
        );
        assert_eq!(
            std::fs::read_to_string(target.minimax_plugin_hooks_path()).unwrap(),
            minimax_plugin_hooks(&target).unwrap()
        );
        assert_eq!(
            std::fs::read(target.minimax_plugin_icon_path()).unwrap(),
            MINIMAX_PLUGIN_ICON
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn minimax_recovers_from_truncated_remote_temporary_files() {
        let dir = std::env::temp_dir().join(format!(
            "tty7-minimax-plugin-temp-truncated-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let host = FakeRemote::shared();
        let target = HookTarget::remote(&*host, dir.clone());
        std::fs::create_dir_all(target.minimax_plugin_root()).unwrap();
        std::fs::write(
            target.minimax_plugin_hooks_temp_path(),
            format!(r#"{{"_tty7":"{MINIMAX_PLUGIN_HOOKS_MARKER}","hooks":{{"#),
        )
        .unwrap();
        std::fs::write(target.minimax_plugin_icon_temp_path(), b"truncated").unwrap();

        install_hooks(&target, HookAgent::MiniMaxCode)
            .expect("truncated temporary files must be recoverable");
        assert_eq!(
            hooks_state(&target, HookAgent::MiniMaxCode),
            HooksState::Installed
        );
        assert!(!target.minimax_plugin_hooks_temp_path().exists());
        assert!(!target.minimax_plugin_icon_temp_path().exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn minimax_recovers_before_the_first_marker_is_written() {
        let dir =
            std::env::temp_dir().join(format!("tty7-minimax-early-write-{}", std::process::id()));
        let host = FakeRemote::shared();
        let target = HookTarget::remote(&*host, dir.clone());
        let hooks = minimax_plugin_hooks(&target).unwrap();
        let marker_end =
            hooks.find(MINIMAX_PLUGIN_HOOKS_MARKER).unwrap() + MINIMAX_PLUGIN_HOOKS_MARKER.len();
        for end in 0..=marker_end {
            std::fs::create_dir_all(target.minimax_plugin_root()).unwrap();
            std::fs::write(
                target.minimax_plugin_hooks_temp_path(),
                &hooks.as_bytes()[..end],
            )
            .unwrap();
            install_hooks(&target, HookAgent::MiniMaxCode).unwrap();
            assert_eq!(
                hooks_state(&target, HookAgent::MiniMaxCode),
                HooksState::Installed
            );
            uninstall_hooks(&target, HookAgent::MiniMaxCode).unwrap();
        }
        std::fs::create_dir_all(target.minimax_plugin_root()).unwrap();
        std::fs::write(target.minimax_plugin_hooks_temp_path(), b"").unwrap();
        std::fs::write(target.minimax_plugin_icon_path(), b"foreign icon").unwrap();
        assert!(install_hooks(&target, HookAgent::MiniMaxCode).is_err());
        assert_eq!(
            std::fs::read(target.minimax_plugin_icon_path()).unwrap(),
            b"foreign icon"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn minimax_local_publish_recovers_from_each_partial_write() {
        let dir =
            std::env::temp_dir().join(format!("tty7-minimax-local-failure-{}", std::process::id()));
        for (index, name) in [
            MINIMAX_PLUGIN_HOOKS_TEMP,
            MINIMAX_PLUGIN_ICON_TEMP,
            MINIMAX_PLUGIN_MANIFEST_TEMP,
        ]
        .into_iter()
        .enumerate()
        {
            let host = FakeRemote(local_host(), Some(name), false.into());
            let target = HookTarget {
                host: &host,
                home: dir.join(index.to_string()),
                exe: dir.join("tty7-app"),
            };
            let temp = target.home.join(name);
            let final_path = target.home.join(format!("published-{index}"));
            let error = minimax_publish_file(&target, &final_path, &temp, b"complete")
                .expect_err("local staging writes must pass through Host");
            assert!(error.to_string().contains("injected partial write failure"));
            assert_eq!(std::fs::read(&temp).unwrap(), b"comp");
            minimax_publish_file(&target, &final_path, &temp, b"complete").unwrap();
            assert_eq!(std::fs::read(&final_path).unwrap(), b"complete");
            let names = std::fs::read_dir(&target.home)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect::<Vec<_>>();
            assert_eq!(names.len(), 1, "only the managed staging file is used");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn minimax_uninstall_rejects_symlinked_plugin_subdirectories() {
        use std::os::unix::fs::symlink;

        let dir = std::env::temp_dir().join(format!(
            "tty7-minimax-plugin-symlink-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let host = FakeRemote::shared();
        let target = HookTarget::remote(&*host, dir.clone());
        install_hooks(&target, HookAgent::MiniMaxCode).unwrap();

        let hooks_path = target.minimax_plugin_hooks_path();
        let hooks_dir = hooks_path.parent().unwrap().to_path_buf();
        let external_hooks = dir.join("external-hooks");
        std::fs::create_dir_all(&external_hooks).unwrap();
        std::fs::copy(&hooks_path, external_hooks.join("tty7.json")).unwrap();
        std::fs::remove_dir_all(&hooks_dir).unwrap();
        symlink(&external_hooks, &hooks_dir).unwrap();

        assert!(uninstall_hooks(&target, HookAgent::MiniMaxCode).is_err());
        assert!(external_hooks.join("tty7.json").exists());
        assert!(target.minimax_plugin_manifest_path().exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn minimax_uninstall_rejects_dangling_plugin_subdirectory_symlinks() {
        use std::os::unix::fs::symlink;

        let dir = std::env::temp_dir().join(format!(
            "tty7-minimax-plugin-dangling-symlink-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let host = FakeRemote::shared();
        let target = HookTarget::remote(&*host, dir.clone());
        install_hooks(&target, HookAgent::MiniMaxCode).unwrap();

        let hooks_path = target.minimax_plugin_hooks_path();
        let hooks_dir = hooks_path.parent().unwrap();
        let missing_hooks = dir.join("missing-hooks");
        std::fs::remove_dir_all(hooks_dir).unwrap();
        symlink(&missing_hooks, hooks_dir).unwrap();

        assert!(uninstall_hooks(&target, HookAgent::MiniMaxCode).is_err());
        assert!(target.minimax_plugin_manifest_path().exists());
        assert_eq!(std::fs::read_link(hooks_dir).unwrap(), missing_hooks);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn minimax_plugin_refuses_foreign_content_during_install_and_uninstall() {
        let dir =
            std::env::temp_dir().join(format!("tty7-minimax-plugin-guard-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let host = FakeRemote::shared();
        let target = HookTarget::remote(&*host, dir.clone());
        let root = target.minimax_plugin_root();
        std::fs::create_dir_all(&root).unwrap();
        let foreign = target.under(&root, &["foreign.txt"]);
        std::fs::write(&foreign, b"keep me").unwrap();

        assert!(install_hooks(&target, HookAgent::MiniMaxCode).is_err());
        assert_eq!(std::fs::read(&foreign).unwrap(), b"keep me");

        std::fs::remove_dir_all(&root).unwrap();
        install_hooks(&target, HookAgent::MiniMaxCode).unwrap();
        std::fs::write(target.minimax_plugin_hooks_path(), b"{\"hooks\":{}}").unwrap();
        assert!(uninstall_hooks(&target, HookAgent::MiniMaxCode).is_err());
        assert!(target.minimax_plugin_manifest_path().exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn qoder_compaction_preserves_the_active_turn() {
        use crate::core::cli_agent::{AgentSessionState, AgentStatus};

        let mut state = AgentSessionState::default();
        state.apply_event(&round_trip(
            "qodercli",
            "prompt-submit",
            r#"{"session_id":"q-1","cwd":"/repo","prompt":"Continue the task"}"#,
        ));
        let before = state.clone();
        let compact = r#"{"session_id":"q-1","cwd":"/repo","source":"compact"}"#;
        if let Some(event) = effective_event("qodercli", "session-start", compact) {
            state.apply_event(&round_trip("qodercli", event, compact));
        }
        assert_eq!(state, before, "compaction must preserve the active turn");

        state.apply_event(&round_trip("qodercli", "tool-complete", "{}"));
        assert_eq!(state.status, AgentStatus::Working);
        state.apply_event(&round_trip("qodercli", "stop", "{}"));
        assert_eq!(state.status, AgentStatus::Done);

        for input in [
            r#"{"source":"startup","message":"compact"}"#,
            r#"{"source":"resume"}"#,
            r#"{"source":"clear"}"#,
            "{}",
            "not JSON",
        ] {
            let event = effective_event("qodercli", "session-start", input)
                .expect("ordinary session starts still reach the state machine");
            let mut session = before.clone();
            session.apply_event(&round_trip("qodercli", event, input));
            assert_eq!(session.status, AgentStatus::Idle, "{input}");
        }
        assert_eq!(
            effective_event("claude", "session-start", compact),
            Some("session-start"),
            "the filter is specific to Qoder"
        );
        assert_eq!(
            effective_event("qodercli", "prompt-submit", compact),
            Some("prompt-submit")
        );
    }

    #[test]
    fn qoder_mcp_elicitation_waits_for_user_input() {
        use crate::core::cli_agent::{AgentSessionState, AgentStatus};

        let apply_hook = |state: &mut AgentSessionState, hook: &str, input: &str| {
            let event = HookAgent::QoderCLI
                .hook_map_events()
                .unwrap()
                .iter()
                .find_map(|(name, event)| (*name == hook).then_some(*event))
                .and_then(|event| effective_event("qodercli", event, input));
            if let Some(event) = event {
                state.apply_event(&round_trip("qodercli", event, input));
            }
        };
        let mut state = AgentSessionState::default();
        apply_hook(
            &mut state,
            "UserPromptSubmit",
            r#"{"session_id":"q-1","prompt":"Look up my tickets"}"#,
        );
        assert_eq!(state.status, AgentStatus::Working);
        // Qoder says outright when it is blocked — `PermissionRequest` and
        // `Elicitation` — so it must not also carry `Notification`, which
        // fires for non-blocking alerts and would strand the pane on
        // "waiting". Asserting the map has no seat for it is the check; a
        // `Notification` payload put through `apply_hook` would be dropped
        // for want of one and prove nothing.
        assert!(
            !HookAgent::QoderCLI
                .hook_map_events()
                .unwrap()
                .iter()
                .any(|(hook, _)| *hook == "Notification"),
            "an unblocking alert must not read as a question"
        );

        // The MCP tool is already authorized, so no PermissionRequest precedes
        // its request for more information from the user.
        apply_hook(
            &mut state,
            "Elicitation",
            r#"{
                "session_id":"q-1",
                "hook_event_name":"Elicitation",
                "mcp_server_name":"tickets",
                "message":"Choose a project",
                "mode":"form"
            }"#,
        );
        assert_eq!(state.status, AgentStatus::Waiting);
        assert_eq!(state.message.as_deref(), Some("Choose a project"));
        assert_eq!(state.session_id.as_deref(), Some("q-1"));

        apply_hook(&mut state, "PostToolUse", r#"{"session_id":"q-1"}"#);
        assert_eq!(state.status, AgentStatus::Working);
        assert_eq!(state.message, None);
        apply_hook(&mut state, "Stop", r#"{"session_id":"q-1"}"#);
        assert_eq!(state.status, AgentStatus::Done);
    }

    /// CodeBuddy reuses Claude Code's hook shape and payload, so a turn walks
    /// the same way — but it fires `SessionStart` with `source: "compact"`
    /// in the middle of a turn, which must not reset the pane to idle.
    #[test]
    fn codebuddy_turns_survive_compaction_and_record_the_session() {
        use crate::core::cli_agent::{AgentSessionState, AgentStatus, CLIAgent};

        let apply_hook = |state: &mut AgentSessionState, hook: &str, input: &str| {
            let event = HookAgent::CodeBuddy
                .hook_map_events()
                .unwrap()
                .iter()
                .find_map(|(name, event)| (*name == hook).then_some(*event))
                .unwrap_or_else(|| panic!("CodeBuddy installs no {hook} hook"));
            if let Some(event) = effective_event("codebuddy", event, input) {
                let ev = round_trip("codebuddy", event, input);
                assert_eq!(ev.agent, Some(CLIAgent::CodeBuddy));
                state.apply_event(&ev);
            }
        };
        let mut state = AgentSessionState::default();
        apply_hook(
            &mut state,
            "SessionStart",
            r#"{"session_id":"cb-1","cwd":"/repo","source":"startup"}"#,
        );
        assert_eq!(state.status, AgentStatus::Idle);
        apply_hook(
            &mut state,
            "UserPromptSubmit",
            r#"{"session_id":"cb-1","cwd":"/repo","prompt":"Fix the build"}"#,
        );
        assert_eq!(state.status, AgentStatus::Working);

        let before = state.clone();
        apply_hook(
            &mut state,
            "SessionStart",
            r#"{"session_id":"cb-1","cwd":"/repo","source":"compact"}"#,
        );
        assert_eq!(state, before, "compaction must preserve the active turn");

        apply_hook(
            &mut state,
            "PermissionRequest",
            r#"{"session_id":"cb-1","tool_name":"Bash"}"#,
        );
        assert_eq!(state.status, AgentStatus::Waiting);
        apply_hook(&mut state, "PostToolUse", r#"{"session_id":"cb-1"}"#);
        assert_eq!(state.status, AgentStatus::Working);
        apply_hook(&mut state, "StopFailure", r#"{"session_id":"cb-1"}"#);
        assert_eq!(state.status, AgentStatus::Done, "a failed turn still ends");
        assert_eq!(state.session_id.as_deref(), Some("cb-1"));
        assert_eq!(state.cwd.as_deref(), Some(Path::new("/repo")));

        assert!(
            !HookAgent::CodeBuddy
                .hook_map_events()
                .unwrap()
                .iter()
                .any(|(hook, _)| *hook == "Notification"),
            "PermissionRequest reports the block; Notification would only muddy it"
        );
    }

    /// Cursor calls the session a conversation, and only its tool events say
    /// where they ran. Both still have to reach the pane, or Copy Session ID
    /// and resume stay dark.
    #[test]
    fn cursor_payloads_name_the_session_and_its_workspace() {
        let ev = round_trip(
            "cursor",
            "stop",
            r#"{"conversation_id":"cur-1","generation_id":"g-1","status":"completed",
                "workspace_roots":["/repo","/other"],"hook_event_name":"stop"}"#,
        );
        assert_eq!(ev.agent, Some(CLIAgent::Cursor));
        assert_eq!(ev.session_id.as_deref(), Some("cur-1"));
        assert_eq!(ev.cwd.as_deref(), Some(Path::new("/repo")));

        let ev = round_trip(
            "cursor",
            "prompt-submit",
            r#"{"conversation_id":"cur-1","cwd":"/repo/sub","workspace_roots":["/repo"],
                "tool_name":"Shell"}"#,
        );
        assert_eq!(
            ev.cwd.as_deref(),
            Some(Path::new("/repo/sub")),
            "a real cwd beats the workspace root"
        );

        let ev = round_trip(
            "cursor",
            "session-start",
            r#"{"session_id":"cur-2","conversation_id":"cur-2","workspace_roots":[]}"#,
        );
        assert_eq!(ev.session_id.as_deref(), Some("cur-2"));
        assert_eq!(ev.cwd, None, "an empty root list names no directory");
    }

    /// A `cursor-agent` turn opens on `beforeSubmitPrompt` and closes on
    /// `stop`; tool calls in between only move the activity counter.
    #[test]
    fn cursor_cli_turns_open_on_submit_and_close_on_stop() {
        use crate::core::cli_agent::{AgentSessionState, AgentStatus};

        let apply_hook = |state: &mut AgentSessionState, hook: &str, input: &str| {
            let event = HookAgent::Cursor
                .hook_map_events()
                .unwrap()
                .iter()
                .find_map(|(name, event)| (*name == hook).then_some(*event))
                .unwrap_or_else(|| panic!("Cursor installs no {hook} hook"));
            let event = effective_event("cursor", event, input).unwrap();
            state.apply_event(&round_trip("cursor", event, input));
        };
        let mut state = AgentSessionState::default();
        apply_hook(
            &mut state,
            "sessionStart",
            r#"{"session_id":"cur-1","conversation_id":"cur-1","workspace_roots":["/repo"]}"#,
        );
        assert_eq!(state.status, AgentStatus::Idle);
        assert_eq!(state.session_id.as_deref(), Some("cur-1"));

        for turn in 1..=2 {
            apply_hook(
                &mut state,
                "beforeSubmitPrompt",
                r#"{"conversation_id":"cur-1","prompt":"Fix the build"}"#,
            );
            assert_eq!(state.status, AgentStatus::Working, "turn {turn}");
            let before = state.activity;
            apply_hook(
                &mut state,
                "postToolUse",
                r#"{"conversation_id":"cur-1","tool_name":"Shell","cwd":"/repo"}"#,
            );
            assert_eq!(state.status, AgentStatus::Working, "turn {turn}");
            assert_eq!(state.activity, before + 1, "a tool call is activity");
            apply_hook(
                &mut state,
                "stop",
                r#"{"conversation_id":"cur-1","status":"completed"}"#,
            );
            assert_eq!(state.status, AgentStatus::Done, "turn {turn}");
            assert_eq!(state.turns, turn, "each turn is counted once");
        }

        // A turn that answers without a tool still shows as working.
        apply_hook(
            &mut state,
            "beforeSubmitPrompt",
            r#"{"conversation_id":"cur-1","prompt":"Explain this"}"#,
        );
        assert_eq!(state.status, AgentStatus::Working);
        apply_hook(
            &mut state,
            "stop",
            r#"{"conversation_id":"cur-1","status":"aborted"}"#,
        );
        assert_eq!(state.status, AgentStatus::Done);

        apply_hook(
            &mut state,
            "sessionEnd",
            r#"{"session_id":"cur-1","conversation_id":"cur-1","reason":"user_close"}"#,
        );
        assert_eq!(state.status, AgentStatus::Idle);
        assert_eq!(state.session_id.as_deref(), Some("cur-1"));
    }

    /// Cursor's `hooks.json` is flat like Crush's, but it is also ignored
    /// outright without `"version": 1` at the root — so a file tty7 creates
    /// has to carry one, and a user's own file keeps whatever it declares.
    #[test]
    fn cursor_hooks_file_is_versioned_flat_and_preserves_user_entries() {
        let host = FakeRemote::shared();
        let base = std::env::temp_dir().join(format!("tty7-cursor-hooks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let target = HookTarget::remote(&*host, base.clone());
        let config = HookAgent::Cursor.target_path(&target);
        let read = || -> serde_json::Value {
            serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap()
        };

        // From nothing: tty7 creates the file, so it declares the version.
        install_hooks(&target, HookAgent::Cursor).expect("install succeeds");
        assert_eq!(
            hooks_state(&target, HookAgent::Cursor),
            HooksState::Installed
        );
        let created = read();
        assert_eq!(created["version"], 1);
        for (hook, event) in CURSOR_HOOK_EVENTS {
            let entries = created["hooks"][*hook].as_array().unwrap();
            assert_eq!(entries.len(), 1, "{hook}");
            assert_eq!(
                entries[0],
                serde_json::json!({ "command": target.hook_command(HookAgent::Cursor, event) }),
                "{hook} is one flat entry naming its emitter"
            );
        }
        uninstall_hooks(&target, HookAgent::Cursor).unwrap();
        assert_eq!(
            hooks_state(&target, HookAgent::Cursor),
            HooksState::NotInstalled
        );

        // A user's file with hooks of their own and a version already set.
        let user_config = serde_json::json!({
            "version": 1,
            "hooks": {
                "stop": [{ "command": "./audit.sh", "timeout": 5 }],
                "beforeShellExecution": [{ "command": "./guard.sh", "matcher": "rm" }]
            }
        });
        std::fs::write(&config, user_config.to_string()).unwrap();
        install_hooks(&target, HookAgent::Cursor).unwrap();
        install_hooks(&target, HookAgent::Cursor).expect("re-install succeeds");
        let merged = read();
        let stop = merged["hooks"]["stop"].as_array().unwrap();
        assert_eq!(
            stop.len(),
            2,
            "one tty7 entry beside the user's after two installs"
        );
        assert_eq!(stop[0], user_config["hooks"]["stop"][0]);
        assert_eq!(
            merged["hooks"]["beforeShellExecution"], user_config["hooks"]["beforeShellExecution"],
            "a permission hook tty7 does not install is left alone"
        );
        assert_eq!(
            uninstall_hooks(&target, HookAgent::Cursor).unwrap(),
            HookOutcome::Removed
        );
        assert_eq!(read(), user_config, "uninstall restores the user's file");

        // Hooks written into a file Cursor would ignore are not installed yet.
        let mut unversioned = serde_json::json!({ "hooks": {} });
        for (hook, event) in CURSOR_HOOK_EVENTS {
            unversioned["hooks"][*hook] = serde_json::json!([
                { "command": target.hook_command(HookAgent::Cursor, event) }
            ]);
        }
        std::fs::write(&config, unversioned.to_string()).unwrap();
        assert_eq!(
            hooks_state(&target, HookAgent::Cursor),
            HooksState::Outdated
        );
        assert_eq!(refresh_hooks(&target), 1);
        assert_eq!(read()["version"], 1);
        assert_eq!(
            hooks_state(&target, HookAgent::Cursor),
            HooksState::Installed
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// Crush's lone `PreToolUse` has no `Stop` to close a turn behind it, so it
    /// must not open one: the pane would read as busy until Crush exits.
    #[test]
    fn crush_tool_calls_record_the_session_without_starting_a_turn() {
        use crate::core::cli_agent::{AgentSessionState, AgentStatus};

        let mut state = AgentSessionState::default();
        for (hook, event) in HookAgent::Crush.hook_map_events().unwrap() {
            assert_eq!(*hook, "PreToolUse");
            let input =
                r#"{"event":"PreToolUse","session_id":"c-1","cwd":"/repo","tool_name":"bash"}"#;
            let event = effective_event("crush", event, input).unwrap();
            state.apply_event(&round_trip("crush", event, input));
        }
        assert_eq!(state.status, AgentStatus::Idle);
        assert_eq!(state.session_id.as_deref(), Some("c-1"));
        assert_eq!(state.cwd.as_deref(), Some(Path::new("/repo")));
        assert_eq!(state.activity, 1);
    }

    /// Qwen is the one agent that reports a blocked turn outright, so it must
    /// not also carry the `Notification` hook the others need — that event fires
    /// for non-blocking alerts too and would strand the pane on "waiting".
    #[test]
    fn qwen_reports_permission_requests_natively() {
        assert!(
            QWEN_HOOK_EVENTS
                .iter()
                .any(|(hook, tty7)| *hook == "PermissionRequest" && *tty7 == "permission-request")
        );
        assert!(
            !QWEN_HOOK_EVENTS
                .iter()
                .any(|(hook, _)| *hook == "Notification")
        );
        assert_eq!(
            effective_event("qwen", "permission-request", "{}"),
            Some("permission-request")
        );
    }

    #[test]
    fn gemini_and_droid_notifications_filter_to_permission_requests() {
        assert_eq!(
            effective_event(
                "gemini",
                "notification",
                r#"{"notification_type":"ToolPermission"}"#
            ),
            Some("permission-request")
        );
        assert_eq!(
            effective_event(
                "droid",
                "notification",
                r#"{"notification_type":"permission_prompt"}"#
            ),
            Some("permission-request")
        );
        // A non-blocking alert must not strand the pane on "waiting".
        for agent in ["gemini", "droid"] {
            assert_eq!(
                effective_event(
                    agent,
                    "notification",
                    r#"{"notification_type":"auth_success"}"#
                ),
                None,
                "{agent} reported an idle notification as a block"
            );
        }
    }

    #[test]
    fn uninstalling_goose_takes_its_generated_plugin_dirs_with_it() {
        let root = std::env::temp_dir().join(format!("tty7-goose-test-{}", std::process::id()));
        let plugins = root.join("plugins");
        let plugin = plugins.join("tty7");
        let hooks_dir = plugin.join("hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        let path = hooks_dir.join("hooks.json");

        let host = local_host();
        let t = HookTarget::local(&*host).expect("home resolves in tests");
        let content = goose_hooks_json(&t).expect("goose content builds");
        assert!(content.contains("agent-hook goose"));
        let marker = "agent-hook goose";

        owned_file_install(&t, &path, &content, marker).expect("install");
        owned_file_uninstall(&t, &path, marker).expect("uninstall");

        assert!(!path.exists());
        assert!(!hooks_dir.exists(), "the generated hooks/ dir goes too");
        assert!(!plugin.exists(), "and the tty7 plugin dir above it");
        assert!(plugins.exists(), "but never the shared plugins/ dir");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn copilot_notifications_filter_to_permission_requests() {
        assert_eq!(
            effective_event("copilot", "notification", r#"{"type":"permission_prompt"}"#),
            Some("permission-request")
        );
        assert_eq!(
            effective_event(
                "copilot",
                "notification",
                r#"{"type":"elicitation_dialog"}"#
            ),
            Some("permission-request")
        );
        assert_eq!(
            effective_event("copilot", "notification", r#"{"type":"turn_summary"}"#),
            None
        );
        assert_eq!(
            effective_event(
                "grok",
                "notification",
                r#"{"notificationType":"elicitation_dialog","message":"User question requested"}"#
            ),
            Some("permission-request")
        );
        for noisy in ["permission_prompt", "task_complete", "agent_error"] {
            assert_eq!(
                effective_event(
                    "grok",
                    "notification",
                    &format!(r#"{{"notificationType":"{noisy}"}}"#)
                ),
                None,
                "grok {noisy} is not a block"
            );
        }
        assert_eq!(
            effective_event("claude", "notification", "{}"),
            Some("notification")
        );
        assert_eq!(effective_event("copilot", "stop", "{}"), Some("stop"));
        assert_eq!(effective_event("grok", "stop", "{}"), Some("stop"));
    }

    #[cfg(unix)]
    #[test]
    fn ancestor_tty_device_is_none_or_a_dev_path() {
        match ancestor_tty_device() {
            None => {}
            Some(dev) => assert!(
                dev.starts_with("/dev/"),
                "a resolved tty must be an openable device path, got {dev:?}"
            ),
        }
    }

    #[test]
    fn marker_detection_matches_our_entries_only() {
        let ours = serde_json::json!({
            "hooks": [{ "type": "command", "command": "\"/x/tty7\" agent-hook claude stop" }]
        });
        assert!(marker_command(&ours, "agent-hook claude").is_some());
        assert!(marker_command(&ours, "agent-hook codex").is_none());
        let theirs = serde_json::json!({
            "hooks": [{ "type": "command", "command": "afplay /System/Library/Sounds/Glass.aiff" }]
        });
        assert!(marker_command(&theirs, "agent-hook claude").is_none());
        assert!(marker_command(&serde_json::json!({}), "agent-hook claude").is_none());

        // Crush flattens the same entry: `command` sits beside `matcher` rather
        // than inside a nested `hooks` array, and the reader has to see it.
        let flat = serde_json::json!({
            "matcher": "^bash$",
            "command": "\"/x/tty7\" agent-hook crush prompt-submit"
        });
        assert_eq!(
            marker_command(&flat, "agent-hook crush"),
            Some("\"/x/tty7\" agent-hook crush prompt-submit")
        );
        assert!(marker_command(&flat, "agent-hook claude").is_none());
    }

    fn local_host() -> crate::host::SharedHost {
        crate::host::local::LocalHost::new()
    }

    struct FakeRemote(
        crate::host::SharedHost,
        Option<&'static str>,
        std::sync::atomic::AtomicBool,
    );

    impl FakeRemote {
        fn shared() -> crate::host::SharedHost {
            std::sync::Arc::new(FakeRemote(local_host(), None, false.into()))
        }
    }

    impl Host for FakeRemote {
        fn id(&self) -> crate::host::HostId {
            if self.1.is_some() {
                return crate::host::HostId::LOCAL;
            }
            crate::host::HostId::from_connection_key("ssh-direct:me@box:22")
        }
        fn separator(&self) -> char {
            '/'
        }
        fn is_absolute(&self, p: &Path) -> bool {
            p.to_string_lossy().starts_with('/')
        }
        fn read_dir(&self, dir: &Path, root: Option<&Path>) -> io::Result<Vec<crate::host::Entry>> {
            self.0.read_dir(dir, root)
        }
        fn stat(&self, p: &Path) -> io::Result<crate::host::Meta> {
            self.0.stat(p)
        }
        fn read_file(&self, p: &Path, max_bytes: u64) -> io::Result<Vec<u8>> {
            self.0.read_file(p, max_bytes)
        }
        fn canonicalize(&self, p: &Path) -> io::Result<PathBuf> {
            self.0.canonicalize(p)
        }
        fn search(
            &self,
            roots: &[PathBuf],
            query: &str,
            limit: usize,
            max_dirs: usize,
            show_hidden: bool,
        ) -> io::Result<Vec<crate::host::SearchHit>> {
            self.0.search(roots, query, limit, max_dirs, show_hidden)
        }
        fn write_file(&self, p: &Path, bytes: &[u8]) -> io::Result<crate::host::Meta> {
            if self
                .1
                .is_some_and(|name| p.file_name() == Some(std::ffi::OsStr::new(name)))
                && !self.2.swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                self.0.write_file(p, &bytes[..bytes.len() / 2])?;
                return Err(io::Error::other("injected partial write failure"));
            }
            self.0.write_file(p, bytes)
        }
        fn create_file_new(&self, p: &Path) -> io::Result<()> {
            self.0.create_file_new(p)
        }
        fn create_dir(&self, p: &Path, recursive: bool) -> io::Result<()> {
            self.0.create_dir(p, recursive)
        }
        fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
            self.0.rename(from, to)
        }
        fn remove(&self, p: &Path, recursive: bool) -> io::Result<()> {
            self.0.remove(p, recursive)
        }
        fn repo_root(&self, p: &Path) -> io::Result<Option<PathBuf>> {
            self.0.repo_root(p)
        }
        fn git(&self, cwd: &Path, args: &[&str]) -> io::Result<crate::host::Output> {
            self.0.git(cwd, args)
        }
        fn shells(&self) -> io::Result<crate::host::ShellInventory> {
            self.0.shells()
        }
        fn watch(&self, dirs: &[PathBuf]) -> io::Result<crate::host::WatchSub> {
            self.0.watch(dirs)
        }
    }

    #[test]
    fn hook_command_quotes_the_exe_path() {
        let host = local_host();
        let target = HookTarget::local(&*host).expect("home resolves in tests");
        let cmd = target.hook_command(HookAgent::Claude, "stop");
        // On Windows the executable may be emitted as a bare PATH-resolvable
        // name: PowerShell cannot invoke a quoted path without the `&` call
        // operator, so quoting is avoided whenever possible.
        #[cfg(not(windows))]
        assert!(cmd.starts_with('"'));
        assert!(cmd.ends_with("agent-hook claude stop"));
    }

    #[test]
    fn remote_paths_are_built_in_the_remote_machine_s_spelling() {
        let host = FakeRemote::shared();
        let target = HookTarget::remote(&*host, PathBuf::from("/home/me"));

        for (agent, expected) in [
            (HookAgent::Claude, "/home/me/.claude/settings.json"),
            (HookAgent::Codex, "/home/me/.codex/hooks.json"),
            (HookAgent::TraeCode, "/home/me/.trae/cli/hooks.json"),
            (HookAgent::Copilot, "/home/me/.copilot/hooks/tty7.json"),
            (
                HookAgent::OpenCode,
                "/home/me/.config/opencode/plugins/tty7.js",
            ),
            (HookAgent::Pi, "/home/me/.pi/agent/extensions/tty7/index.ts"),
            (HookAgent::Grok, "/home/me/.grok/hooks/tty7.json"),
            (
                HookAgent::OhMyPi,
                "/home/me/.omp/agent/extensions/tty7/index.ts",
            ),
            (HookAgent::Kimi, "/home/me/.kimi-code/config.toml"),
            (HookAgent::QoderCLI, "/home/me/.qoder/settings.json"),
            (HookAgent::Crush, "/home/me/.config/crush/crush.json"),
            (HookAgent::CodeBuddy, "/home/me/.codebuddy/settings.json"),
            (HookAgent::Cursor, "/home/me/.cursor/hooks.json"),
        ] {
            assert_eq!(
                agent.target_path(&target),
                PathBuf::from(expected),
                "{agent:?} target path"
            );
            assert_eq!(
                agent.target_display(&target),
                expected.replacen("/home/me/", "~/", 1),
                "{agent:?} display path"
            );
        }
        assert_eq!(
            HookAgent::MiniMaxCode.target_path(&target),
            PathBuf::from("/home/me/.minimax/plugins/tty7-agent-hooks/.minimax-plugin/plugin.json")
        );
        assert_eq!(
            HookAgent::MiniMaxCode.target_display(&target),
            "~/.minimax/plugins/tty7-agent-hooks"
        );
    }

    #[test]
    fn the_hook_command_names_the_binary_on_that_machine() {
        let host = FakeRemote::shared();
        let target = HookTarget::remote(&*host, PathBuf::from("/home/me"));
        let dialect = crate::daemon::install::RemoteProtocol::of_this_build();
        let name = format!("tty7-server-c{}p{}", dialect.control, dialect.protocol);
        assert_eq!(
            target.hook_command(HookAgent::Claude, "stop"),
            format!("\"/home/me/.local/share/tty7/bin/{name}\" agent-hook claude stop")
        );

        let local = local_host();
        let here = HookTarget::local(&*local).expect("home resolves in tests");
        let exe = std::env::current_exe().unwrap();
        let command_exe = here
            .hook_command_exe()
            .unwrap_or_else(|| format!("\"{}\"", exe.display()));
        assert_eq!(
            here.hook_command(HookAgent::Claude, "stop"),
            format!("{command_exe} agent-hook claude stop")
        );
    }

    #[test]
    fn a_remote_install_round_trips_through_the_host() {
        let dir = std::env::temp_dir().join(format!("tty7-remote-hooks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let host = FakeRemote::shared();
        let target = HookTarget::remote(&*host, dir.clone());

        for agent in [HookAgent::Claude, HookAgent::Grok, HookAgent::OhMyPi] {
            assert_eq!(hooks_state(&target, agent), HooksState::NotInstalled);
            install_hooks(&target, agent).expect("install succeeds");
            assert_eq!(hooks_state(&target, agent), HooksState::Installed);
            let path = agent.target_path(&target);
            let dialect = crate::daemon::install::RemoteProtocol::of_this_build();
            assert!(std::fs::read_to_string(&path).unwrap().contains(&format!(
                "tty7-server-c{}p{}",
                dialect.control, dialect.protocol
            )));
            uninstall_hooks(&target, agent).expect("uninstall succeeds");
            assert_eq!(hooks_state(&target, agent), HooksState::NotInstalled);
        }

        let summary = install_hooks(&target, HookAgent::Codex).expect("codex install succeeds");
        assert_eq!(
            summary,
            HookOutcome::InstalledEnableCodexThere,
            "a remote codex install has to hand the leftover step back to the caller"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn owned_file_contents_carry_marker_and_exe() {
        let exe_raw = std::env::current_exe().unwrap().display().to_string();
        let exe_json = serde_json::to_string(&exe_raw).unwrap();
        let exe = exe_json.trim_matches('"').to_string();
        let host = local_host();
        let target = HookTarget::local(&*host).expect("home resolves in tests");
        let hook_exe_raw = target.hook_command_exe().unwrap_or_else(|| exe_raw.clone());
        let hook_exe_json = serde_json::to_string(&hook_exe_raw).unwrap();
        let hook_exe = hook_exe_json.trim_matches('"');

        let copilot = copilot_hooks_json(&target).expect("copilot content builds");
        let parsed: serde_json::Value = serde_json::from_str(&copilot).expect("valid JSON");
        for event in [
            "sessionStart",
            "userPromptSubmitted",
            "agentStop",
            "sessionEnd",
            "notification",
        ] {
            assert!(
                parsed["hooks"][event][0]["bash"]
                    .as_str()
                    .is_some_and(|c| c.contains("agent-hook copilot")),
                "copilot {event} carries the emitter"
            );
        }
        assert!(copilot.contains(hook_exe));

        let opencode = opencode_plugin_js(&target).expect("opencode content builds");
        assert!(opencode.contains("agent-hook opencode"));
        assert!(opencode.contains(hook_exe));
        assert!(opencode.contains(r#"process.env["TTY7"]"#));
        for (needle, message) in [
            (
                "properties.sessionID",
                "opencode captures the session id from event properties",
            ),
            (
                r#"session_id: sessionId"#,
                "opencode forwards the session id to the emitter",
            ),
            (
                "session.status",
                "opencode maps session.status busy/idle to prompt-submit/stop",
            ),
            (
                "session.idle",
                "opencode still maps the session.idle event to stop",
            ),
            (
                "info.parentID",
                "opencode tells a subagent's child session apart from the pane's own",
            ),
            (
                "children.has(properties.sessionID)",
                "opencode lets a child session's events pass without touching the pane",
            ),
        ] {
            assert!(opencode.contains(needle), "{message}");
        }

        for (agent, slug, package) in [
            (HookAgent::Pi, "pi", "@mariozechner/pi-coding-agent"),
            (HookAgent::OhMyPi, "omp", "@oh-my-pi/pi-coding-agent"),
        ] {
            let bridge =
                pi_extension_ts(&target, agent).unwrap_or_else(|| panic!("{slug} content builds"));
            assert!(bridge.contains(&format!("agent-hook {slug}")));
            assert!(bridge.contains(&format!(r#"["agent-hook", "{slug}", event]"#)));
            assert!(bridge.contains(&format!(r#"from "{package}""#)));
            assert!(bridge.contains(&exe));
            assert!(bridge.contains(r#"process.env["TTY7"]"#));
            assert!(bridge.contains("getSessionId"));
            assert!(bridge.contains("session_id"));
            assert!(bridge.contains(r#"stdio: ["pipe", "ignore", "ignore"]"#));
            for event in [
                "session_start",
                "agent_start",
                "agent_end",
                "session_shutdown",
                "ui_prompt_start",
                "ui_prompt_end",
            ] {
                assert!(
                    bridge.contains(&format!(r#"pi.on("{event}""#)),
                    "{slug} bridge subscribes to {event}"
                );
            }
            assert!(
                bridge.contains(r#"pi.on("ui_prompt_end", (_event, ctx) => emit(turn, ctx))"#),
                "{slug} restores the pre-prompt status rather than forcing working"
            );
        }
        assert!(
            pi_extension_ts(&target, HookAgent::Claude).is_none(),
            "only the two Pi-shaped agents get this bridge"
        );

        let grok = grok_hooks_json(&target).expect("grok content builds");
        let parsed: serde_json::Value = serde_json::from_str(&grok).expect("valid JSON");
        for (event, sentinel, matcher) in GROK_HOOK_EVENTS {
            let group = &parsed["hooks"][*event][0];
            let cmd = group["hooks"][0]["command"]
                .as_str()
                .unwrap_or_else(|| panic!("grok {event} carries a command"));
            assert!(
                cmd.ends_with(&format!("agent-hook grok {sentinel}")),
                "grok {event} runs the emitter with {sentinel}, got {cmd}"
            );
            assert_eq!(
                group.get("matcher").and_then(|m| m.as_str()),
                *matcher,
                "grok {event} matcher"
            );
        }
        assert!(grok.contains(hook_exe));
    }

    #[test]
    fn owned_file_round_trip_and_ownership_guard() {
        let dir = std::env::temp_dir().join(format!("tty7-owned-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tty7.json");
        let marker = "agent-hook copilot";
        let host = local_host();
        let t = HookTarget::local(&*host).expect("home resolves in tests");
        let content = copilot_hooks_json(&t).unwrap();

        assert_eq!(
            owned_file_state(&t, &path, &content, marker),
            HooksState::NotInstalled
        );
        owned_file_install(&t, &path, &content, marker).expect("fresh install succeeds");
        assert_eq!(
            owned_file_state(&t, &path, &content, marker),
            HooksState::Installed
        );

        std::fs::write(&path, content.replace(marker, "agent-hook copilot --old")).unwrap();
        assert_eq!(
            owned_file_state(&t, &path, &content, marker),
            HooksState::Outdated
        );
        owned_file_install(&t, &path, &content, marker).expect("reinstall over our own file");
        assert_eq!(
            owned_file_state(&t, &path, &content, marker),
            HooksState::Installed
        );

        std::fs::write(&path, "// my own hooks, hands off").unwrap();
        assert!(owned_file_install(&t, &path, &content, marker).is_err());
        assert!(owned_file_uninstall(&t, &path, marker).is_err());

        std::fs::write(&path, &content).unwrap();
        owned_file_uninstall(&t, &path, marker).expect("uninstall succeeds");
        assert!(!path.exists());
        owned_file_uninstall(&t, &path, marker).expect("uninstall is idempotent");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn qoder_config_dir_controls_local_hook_lifecycle() {
        const CASE_ENV: &str = "TTY7_TEST_QODER_CONFIG_CASE";
        const ROOT_ENV: &str = "TTY7_TEST_QODER_CONFIG_ROOT";
        let Ok(case) = std::env::var(CASE_ENV) else {
            // Each case gets its own environment, without changing the one
            // shared by the other tests or touching the user's settings.
            for case in ["override", "empty", "unset"] {
                let sandbox = tempfile::tempdir().unwrap();
                let mut child = std::process::Command::new(std::env::current_exe().unwrap());
                child
                    .args([
                        "--exact",
                        "core::agent_hooks::tests::qoder_config_dir_controls_local_hook_lifecycle",
                        "--nocapture",
                    ])
                    .env(CASE_ENV, case)
                    .env(ROOT_ENV, sandbox.path());
                match case {
                    "override" => {
                        child.env("QODER_CONFIG_DIR", sandbox.path().join("custom config"))
                    }
                    "empty" => child.env("QODER_CONFIG_DIR", ""),
                    _ => child.env_remove("QODER_CONFIG_DIR"),
                };
                let output = crate::core::proc::output_within(
                    crate::core::proc::hide_console(&mut child),
                    std::time::Duration::from_secs(30),
                )
                .expect("run the isolated Qoder hook test");
                assert!(
                    output.status.success(),
                    "{case}:\n{}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr),
                );
            }
            return;
        };

        let root = PathBuf::from(std::env::var_os(ROOT_ENV).unwrap());
        let host = local_host();
        let target = HookTarget {
            host: &*host,
            home: root.join("home"),
            exe: std::env::current_exe().unwrap(),
        };
        let default_settings = target.home.join(".qoder").join("settings.json");
        let custom_settings = root.join("custom config").join("settings.json");
        let (settings, untouched) = if case == "override" {
            (&custom_settings, &default_settings)
        } else {
            (&default_settings, &custom_settings)
        };
        let user_config = serde_json::json!({
            "model": "qoder-test",
            "hooks": {
                "Stop": [{ "hooks": [{ "type": "command", "command": "echo user-hook" }] }]
            }
        });
        for path in [settings, untouched] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, user_config.to_string()).unwrap();
        }

        let agent = HookAgent::QoderCLI;
        assert_eq!(agent.target_path(&target), *settings);
        let remote_host = FakeRemote::shared();
        let remote = HookTarget::remote(&*remote_host, PathBuf::from("/home/me"));
        assert_eq!(
            agent.target_path(&remote),
            PathBuf::from("/home/me/.qoder/settings.json"),
            "a local override must not redirect remote hooks"
        );

        assert_eq!(hooks_state(&target, agent), HooksState::NotInstalled);
        assert_eq!(
            install_hooks(&target, agent).unwrap(),
            HookOutcome::Installed
        );
        assert_eq!(hooks_state(&target, agent), HooksState::Installed);
        assert!(
            std::fs::read_to_string(settings)
                .unwrap()
                .contains("agent-hook qodercli")
        );
        assert_eq!(
            uninstall_hooks(&target, agent).unwrap(),
            HookOutcome::Removed
        );
        assert_eq!(hooks_state(&target, agent), HooksState::NotInstalled);
        for path in [settings, untouched] {
            let actual: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
            assert_eq!(actual, user_config, "{}", path.display());
        }
    }

    #[test]
    fn codebuddy_config_dir_controls_local_hook_lifecycle() {
        const CASE_ENV: &str = "TTY7_TEST_CODEBUDDY_CONFIG_CASE";
        const ROOT_ENV: &str = "TTY7_TEST_CODEBUDDY_CONFIG_ROOT";
        let Ok(case) = std::env::var(CASE_ENV) else {
            // Each case gets its own environment, without changing the one
            // shared by the other tests or touching the user's settings.
            for case in ["override", "blank", "unset"] {
                let sandbox = tempfile::tempdir().unwrap();
                let mut child = std::process::Command::new(std::env::current_exe().unwrap());
                child
                    .args([
                        "--exact",
                        "core::agent_hooks::tests::codebuddy_config_dir_controls_local_hook_lifecycle",
                        "--nocapture",
                    ])
                    .env(CASE_ENV, case)
                    .env(ROOT_ENV, sandbox.path());
                match case {
                    "override" => {
                        child.env("CODEBUDDY_CONFIG_DIR", sandbox.path().join("custom config"))
                    }
                    // CodeBuddy trims the value, so whitespace is as unset as empty.
                    "blank" => child.env("CODEBUDDY_CONFIG_DIR", "  "),
                    _ => child.env_remove("CODEBUDDY_CONFIG_DIR"),
                };
                let output = crate::core::proc::output_within(
                    crate::core::proc::hide_console(&mut child),
                    std::time::Duration::from_secs(30),
                )
                .expect("run the isolated CodeBuddy hook test");
                assert!(
                    output.status.success(),
                    "{case}:\n{}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr),
                );
            }
            return;
        };

        let root = PathBuf::from(std::env::var_os(ROOT_ENV).unwrap());
        let host = local_host();
        let target = HookTarget {
            host: &*host,
            home: root.join("home"),
            exe: std::env::current_exe().unwrap(),
        };
        let default_settings = target.home.join(".codebuddy").join("settings.json");
        let custom_settings = root.join("custom config").join("settings.json");
        let (settings, untouched) = if case == "override" {
            (&custom_settings, &default_settings)
        } else {
            (&default_settings, &custom_settings)
        };
        let user_config = serde_json::json!({
            "model": "codebuddy-test",
            "hooks": {
                "Stop": [{ "hooks": [{ "type": "command", "command": "echo user-hook" }] }]
            }
        });
        for path in [settings, untouched] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, user_config.to_string()).unwrap();
        }

        let agent = HookAgent::CodeBuddy;
        assert_eq!(agent.target_path(&target), *settings);
        let remote_host = FakeRemote::shared();
        let remote = HookTarget::remote(&*remote_host, PathBuf::from("/home/me"));
        assert_eq!(
            agent.target_path(&remote),
            PathBuf::from("/home/me/.codebuddy/settings.json"),
            "a local override must not redirect remote hooks"
        );

        assert_eq!(hooks_state(&target, agent), HooksState::NotInstalled);
        assert_eq!(
            install_hooks(&target, agent).unwrap(),
            HookOutcome::Installed
        );
        assert_eq!(hooks_state(&target, agent), HooksState::Installed);
        assert!(
            std::fs::read_to_string(settings)
                .unwrap()
                .contains("agent-hook codebuddy")
        );
        assert_eq!(
            uninstall_hooks(&target, agent).unwrap(),
            HookOutcome::Removed
        );
        assert_eq!(hooks_state(&target, agent), HooksState::NotInstalled);
        for path in [settings, untouched] {
            let actual: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
            assert_eq!(actual, user_config, "{}", path.display());
        }
    }

    /// Crush's hook map is one list under `hooks.PreToolUse` like Claude's, but
    /// each entry carries `command` and `matcher` directly instead of wrapping
    /// them in a nested `hooks` array. The merge has to write the flat shape and
    /// still leave a user's own hooks and the rest of `crush.json` alone.
    #[test]
    fn crush_installs_a_flat_hook_and_preserves_user_entries() {
        let host = FakeRemote::shared();
        let base = std::env::temp_dir().join(format!("tty7-crush-hooks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let target = HookTarget::remote(&*host, base.clone());
        let config = HookAgent::Crush.target_path(&target);
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        let user_config = serde_json::json!({
            "model": "crush-test",
            "hooks": {
                "PreToolUse": [
                    { "matcher": "^bash$", "command": "echo user-hook", "timeout": 5 }
                ]
            }
        });
        std::fs::write(&config, serde_json::to_string_pretty(&user_config).unwrap()).unwrap();

        assert_eq!(
            hooks_state(&target, HookAgent::Crush),
            HooksState::NotInstalled
        );
        install_hooks(&target, HookAgent::Crush).expect("install succeeds");
        assert_eq!(
            hooks_state(&target, HookAgent::Crush),
            HooksState::Installed
        );
        install_hooks(&target, HookAgent::Crush).expect("re-install succeeds");

        let merged: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
        assert_eq!(merged["model"], "crush-test");
        let entries = merged["hooks"]["PreToolUse"].as_array().unwrap();
        let ours: Vec<&serde_json::Value> = entries
            .iter()
            .filter(|e| {
                e.get("command")
                    .and_then(|c| c.as_str())
                    .is_some_and(|c| c.contains("agent-hook crush"))
            })
            .collect();
        assert_eq!(ours.len(), 1, "exactly one tty7 entry after two installs");
        assert_eq!(
            ours[0]["command"].as_str(),
            Some(
                target
                    .hook_command(HookAgent::Crush, "tool-complete")
                    .as_str()
            ),
            "the entry is flat and names the tool-complete emitter"
        );
        assert!(
            ours[0].get("hooks").is_none(),
            "Crush does not nest a hooks array inside the entry"
        );
        assert!(
            entries.iter().any(|e| e
                .get("command")
                .and_then(|c| c.as_str())
                .is_some_and(|c| c.contains("user-hook"))),
            "the user's own PreToolUse hook survives"
        );

        assert_eq!(
            uninstall_hooks(&target, HookAgent::Crush).unwrap(),
            HookOutcome::Removed
        );
        assert_eq!(
            hooks_state(&target, HookAgent::Crush),
            HooksState::NotInstalled
        );
        let after: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
        assert_eq!(
            after, user_config,
            "uninstall restores the user's file exactly"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn crush_global_config_controls_local_hook_lifecycle() {
        const CASE_ENV: &str = "TTY7_TEST_CRUSH_CONFIG_CASE";
        const ROOT_ENV: &str = "TTY7_TEST_CRUSH_CONFIG_ROOT";
        let Ok(case) = std::env::var(CASE_ENV) else {
            // Each case gets its own environment, without changing the one
            // shared by the other tests or touching the user's settings.
            for case in ["override", "empty", "unset"] {
                let sandbox = tempfile::tempdir().unwrap();
                let mut child = std::process::Command::new(std::env::current_exe().unwrap());
                child
                    .args([
                        "--exact",
                        "core::agent_hooks::tests::crush_global_config_controls_local_hook_lifecycle",
                        "--nocapture",
                    ])
                    .env(CASE_ENV, case)
                    .env(ROOT_ENV, sandbox.path())
                    // `crush_settings_path` falls back to `$XDG_CONFIG_HOME`
                    // before the sandbox home, and CI exports it. Neutralise it
                    // so the default lands under the home this test owns.
                    .env_remove("XDG_CONFIG_HOME");
                match case {
                    "override" => {
                        child.env("CRUSH_GLOBAL_CONFIG", sandbox.path().join("custom config"))
                    }
                    "empty" => child.env("CRUSH_GLOBAL_CONFIG", ""),
                    _ => child.env_remove("CRUSH_GLOBAL_CONFIG"),
                };
                let output = crate::core::proc::output_within(
                    crate::core::proc::hide_console(&mut child),
                    std::time::Duration::from_secs(30),
                )
                .expect("run the isolated Crush hook test");
                assert!(
                    output.status.success(),
                    "{case}:\n{}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr),
                );
            }
            return;
        };

        let root = PathBuf::from(std::env::var_os(ROOT_ENV).unwrap());
        let host = local_host();
        let target = HookTarget {
            host: &*host,
            home: root.join("home"),
            exe: std::env::current_exe().unwrap(),
        };
        let default_config = target.home.join(".config").join("crush").join("crush.json");
        let custom_config = root.join("custom config").join("crush.json");
        let (config, untouched) = if case == "override" {
            (&custom_config, &default_config)
        } else {
            (&default_config, &custom_config)
        };
        let user_config = serde_json::json!({
            "model": "crush-test",
            "hooks": {
                "PreToolUse": [
                    { "command": "echo user-hook" }
                ]
            }
        });
        for path in [config, untouched] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, serde_json::to_string(&user_config).unwrap()).unwrap();
        }

        let agent = HookAgent::Crush;
        assert_eq!(agent.target_path(&target), *config);
        let remote_host = FakeRemote::shared();
        let remote = HookTarget::remote(&*remote_host, PathBuf::from("/home/me"));
        assert_eq!(
            agent.target_path(&remote),
            PathBuf::from("/home/me/.config/crush/crush.json"),
            "a local override must not redirect remote hooks"
        );

        assert_eq!(hooks_state(&target, agent), HooksState::NotInstalled);
        assert_eq!(
            install_hooks(&target, agent).unwrap(),
            HookOutcome::Installed
        );
        assert_eq!(hooks_state(&target, agent), HooksState::Installed);
        assert!(
            std::fs::read_to_string(config)
                .unwrap()
                .contains("agent-hook crush")
        );
        assert_eq!(
            uninstall_hooks(&target, agent).unwrap(),
            HookOutcome::Removed
        );
        assert_eq!(hooks_state(&target, agent), HooksState::NotInstalled);
        for path in [config, untouched] {
            let actual: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
            assert_eq!(actual, user_config, "{}", path.display());
        }
    }

    #[test]
    fn install_is_idempotent_and_preserves_user_hooks() {
        let dir = std::env::temp_dir().join(format!("tty7-hooks-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let settings = dir.join("settings.json");
        std::fs::write(
            &settings,
            serde_json::json!({
                "model": "opus",
                "hooks": {
                    "Stop": [{ "hooks": [{ "type": "command", "command": "afplay ding.aiff" }] }]
                }
            })
            .to_string(),
        )
        .unwrap();
        unsafe { std::env::set_var("CLAUDE_CONFIG_DIR", &dir) };

        let host = local_host();
        let t = HookTarget::local(&*host).expect("home resolves in tests");
        let remote_host = FakeRemote::shared();
        let remote = HookTarget::remote(&*remote_host, PathBuf::from("/home/me"));
        assert_eq!(
            HookAgent::Claude.target_path(&remote),
            PathBuf::from("/home/me/.claude/settings.json")
        );

        assert_eq!(hooks_state(&t, HookAgent::Claude), HooksState::NotInstalled);
        install_hooks(&t, HookAgent::Claude).expect("install succeeds");
        assert_eq!(hooks_state(&t, HookAgent::Claude), HooksState::Installed);

        install_hooks(&t, HookAgent::Claude).expect("re-install succeeds");
        let root: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        assert_eq!(root["model"], "opus");
        let stop = root["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(
            stop.iter()
                .filter(|m| marker_command(m, "agent-hook claude").is_some())
                .count(),
            1,
            "exactly one tty7 entry after two installs"
        );
        assert!(
            stop.iter()
                .any(|m| m.to_string().contains("afplay ding.aiff")),
            "the user's own Stop hook survives"
        );
        for (event, _) in CLAUDE_HOOK_EVENTS {
            assert!(
                root["hooks"][*event]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|m| marker_command(m, "agent-hook claude").is_some()),
                "{event} carries the tty7 hook"
            );
        }

        let healthy = std::fs::read_to_string(&settings).unwrap();
        std::fs::write(
            &settings,
            healthy.replace("agent-hook claude stop", "agent-hook claude stop --stale"),
        )
        .unwrap();
        assert_eq!(hooks_state(&t, HookAgent::Claude), HooksState::Outdated);
        install_hooks(&t, HookAgent::Claude).expect("reinstall over an outdated entry succeeds");
        assert_eq!(hooks_state(&t, HookAgent::Claude), HooksState::Installed);

        uninstall_hooks(&t, HookAgent::Claude).expect("uninstall succeeds");
        assert_eq!(hooks_state(&t, HookAgent::Claude), HooksState::NotInstalled);
        let root: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        assert_eq!(root["model"], "opus");
        let stop = root["hooks"]["Stop"].as_array().unwrap();
        assert!(
            stop.iter()
                .any(|m| m.to_string().contains("afplay ding.aiff")),
            "the user's own Stop hook survives uninstall"
        );
        assert!(
            root["hooks"].get("SessionStart").is_none(),
            "an event list that held only the tty7 hook is dropped"
        );
        uninstall_hooks(&t, HookAgent::Claude).expect("uninstall is idempotent");

        unsafe { std::env::remove_var("CLAUDE_CONFIG_DIR") };
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Kimi's hooks share `config.toml` with the user's providers and models,
    /// so the merge must leave everything that is not ours — including
    /// comments and formatting — byte-for-byte alone.
    #[test]
    fn kimi_install_preserves_the_user_s_config_toml() {
        let dir = std::env::temp_dir().join(format!("tty7-kimi-hooks-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let config = dir.join("config.toml");
        let user_half = concat!(
            "# my providers\n",
            "default_model = \"kimi-k2\"\n",
            "\n",
            "[[hooks]]\n",
            "event = \"Stop\"\n",
            "command = \"afplay ding.aiff\"\n",
        );
        std::fs::write(&config, user_half).unwrap();
        unsafe { std::env::set_var("KIMI_CODE_HOME", &dir) };

        let host = local_host();
        let t = HookTarget::local(&*host).expect("home resolves in tests");
        assert_eq!(HookAgent::Kimi.target_path(&t), config);

        assert_eq!(hooks_state(&t, HookAgent::Kimi), HooksState::NotInstalled);
        install_hooks(&t, HookAgent::Kimi).expect("install succeeds");
        assert_eq!(hooks_state(&t, HookAgent::Kimi), HooksState::Installed);
        install_hooks(&t, HookAgent::Kimi).expect("re-install succeeds");

        let written = std::fs::read_to_string(&config).unwrap();
        assert!(
            written.starts_with(user_half),
            "the user's half of config.toml — comment included — survives untouched"
        );
        let doc: toml_edit::DocumentMut = written.parse().expect("still valid TOML");
        let hooks = doc["hooks"].as_array_of_tables().unwrap();
        assert_eq!(
            hooks
                .iter()
                .filter(|e| toml_command_is_marked(e, "agent-hook kimi"))
                .count(),
            KIMI_HOOK_EVENTS.len(),
            "exactly one tty7 entry per event after two installs"
        );
        for (event, _) in KIMI_HOOK_EVENTS {
            assert!(
                hooks.iter().any(|e| {
                    toml_command_is_marked(e, "agent-hook kimi")
                        && e.get("event").and_then(|v| v.as_str()) == Some(*event)
                }),
                "{event} carries the tty7 hook"
            );
        }

        let healthy = std::fs::read_to_string(&config).unwrap();
        std::fs::write(
            &config,
            healthy.replace("agent-hook kimi stop", "agent-hook kimi stop --stale"),
        )
        .unwrap();
        assert_eq!(hooks_state(&t, HookAgent::Kimi), HooksState::Outdated);
        install_hooks(&t, HookAgent::Kimi).expect("reinstall over an outdated entry succeeds");
        assert_eq!(hooks_state(&t, HookAgent::Kimi), HooksState::Installed);

        uninstall_hooks(&t, HookAgent::Kimi).expect("uninstall succeeds");
        assert_eq!(hooks_state(&t, HookAgent::Kimi), HooksState::NotInstalled);
        let after = std::fs::read_to_string(&config).unwrap();
        assert!(
            after.contains("afplay ding.aiff"),
            "the user's own Stop hook survives uninstall"
        );
        assert!(!after.contains("agent-hook kimi"));
        uninstall_hooks(&t, HookAgent::Kimi).expect("uninstall is idempotent");

        std::fs::write(&config, "not = valid = toml").unwrap();
        assert!(
            install_hooks(&t, HookAgent::Kimi).is_err(),
            "a config.toml that does not parse is left alone"
        );
        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            "not = valid = toml",
            "and is not rewritten on the way out"
        );
        assert!(
            uninstall_hooks(&t, HookAgent::Kimi).is_err(),
            "uninstall refuses the same file"
        );
        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            "not = valid = toml"
        );

        unsafe { std::env::remove_var("KIMI_CODE_HOME") };
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A `config.toml` that parses but spells `hooks` as something other than
    /// an array of tables is a file we do not understand. Every one of these
    /// must come back as a refusal with the file untouched — the one thing
    /// that must never happen to the file holding the user's API keys is a
    /// silent rewrite.
    #[test]
    fn kimi_refuses_a_hooks_key_of_the_wrong_toml_type() {
        let host = FakeRemote::shared();
        let base = std::env::temp_dir().join(format!("tty7-kimi-shapes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);

        for (name, text) in [
            ("a_string", "hooks = \"nope\"\n"),
            ("a_table", "[hooks]\nfoo = 1\n"),
            (
                "an_inline_array",
                "hooks = [{ event = \"Stop\", command = \"afplay a.aiff\" }]\n",
            ),
        ] {
            let home = base.join(name);
            let t = HookTarget::remote(&*host, home.clone());
            let config = HookAgent::Kimi.target_path(&t);
            std::fs::create_dir_all(config.parent().unwrap()).unwrap();
            std::fs::write(&config, text).unwrap();

            assert_eq!(
                hooks_state(&t, HookAgent::Kimi),
                HooksState::NotInstalled,
                "{name}: nothing of ours is in there"
            );
            assert!(
                install_hooks(&t, HookAgent::Kimi).is_err(),
                "{name}: install refuses"
            );
            assert_eq!(
                uninstall_hooks(&t, HookAgent::Kimi).unwrap(),
                HookOutcome::NoTty7Hooks,
                "{name}: uninstall finds nothing of ours"
            );
            assert_eq!(
                std::fs::read_to_string(&config).unwrap(),
                text,
                "{name}: the file is byte-for-byte what it was"
            );
        }

        // `hooks = []` is the one shape that carries no configuration at all,
        // so it is promoted instead of refused.
        let home = base.join("an_empty_array");
        let t = HookTarget::remote(&*host, home.clone());
        let config = HookAgent::Kimi.target_path(&t);
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(&config, "model = \"k2\"\nhooks = []\n").unwrap();
        install_hooks(&t, HookAgent::Kimi).expect("an empty inline array is promoted, not refused");
        assert_eq!(hooks_state(&t, HookAgent::Kimi), HooksState::Installed);
        let written = std::fs::read_to_string(&config).unwrap();
        assert!(written.starts_with("model = \"k2\"\n"), "{written}");
        assert!(!written.contains("hooks = []"), "{written}");
        assert_eq!(
            uninstall_hooks(&t, HookAgent::Kimi).unwrap(),
            HookOutcome::Removed
        );
        assert!(
            !std::fs::read_to_string(&config).unwrap().contains("hooks"),
            "the key goes when the last entry in it does"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// The rest of the TOML merge contract: a file that does not exist yet, a
    /// second install that changes nothing, entries a hand-edit has mangled,
    /// and an uninstall that has to thread its removals between the user's own
    /// entries and the tables that follow them.
    #[test]
    fn kimi_toml_merge_holds_up_across_the_awkward_shapes() {
        let host = FakeRemote::shared();
        let base = std::env::temp_dir().join(format!("tty7-kimi-merge-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let marker = "agent-hook kimi";

        // Nothing there at all: install creates the directory and the file.
        let t = HookTarget::remote(&*host, base.join("fresh"));
        let config = HookAgent::Kimi.target_path(&t);
        assert!(!config.exists());
        assert_eq!(hooks_state(&t, HookAgent::Kimi), HooksState::NotInstalled);
        assert_eq!(
            uninstall_hooks(&t, HookAgent::Kimi).unwrap(),
            HookOutcome::NothingInstalled,
            "there is no file to take anything out of"
        );
        install_hooks(&t, HookAgent::Kimi).expect("install writes a fresh config.toml");
        assert_eq!(hooks_state(&t, HookAgent::Kimi), HooksState::Installed);
        let once = std::fs::read_to_string(&config).unwrap();
        install_hooks(&t, HookAgent::Kimi).expect("re-install succeeds");
        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            once,
            "a second install is byte-for-byte the first — no churn, no growth"
        );

        // A hand-edit that drops `event` leaves an entry that is ours and is
        // broken. That is Outdated, not NotInstalled: `refresh_hooks` only
        // ever revisits Outdated, so anything else hides the damage.
        let mangled = once.replacen("event = \"SessionStart\"\n", "", 1);
        assert_ne!(mangled, once);
        std::fs::write(&config, &mangled).unwrap();
        assert_eq!(hooks_state(&t, HookAgent::Kimi), HooksState::Outdated);
        install_hooks(&t, HookAgent::Kimi).expect("install repairs it");
        assert_eq!(hooks_state(&t, HookAgent::Kimi), HooksState::Installed);
        assert_eq!(std::fs::read_to_string(&config).unwrap(), once);

        // One marked entry too many is Outdated too, and install prunes it.
        // This one has no `event` at all, so counting only the entries that
        // still name one would find the full roster and call it Installed
        // while a broken ninth entry sat there.
        let mut extra = std::fs::read_to_string(&config).unwrap();
        extra.push_str("\n[[hooks]]\ncommand = \"tty7 agent-hook kimi stop\"\n");
        std::fs::write(&config, &extra).unwrap();
        assert_eq!(hooks_state(&t, HookAgent::Kimi), HooksState::Outdated);
        install_hooks(&t, HookAgent::Kimi).expect("install prunes the stray");
        assert_eq!(hooks_state(&t, HookAgent::Kimi), HooksState::Installed);
        let doc: toml_edit::DocumentMut =
            std::fs::read_to_string(&config).unwrap().parse().unwrap();
        assert_eq!(
            doc["hooks"]
                .as_array_of_tables()
                .unwrap()
                .iter()
                .filter(|e| toml_command_is_marked(e, marker))
                .count(),
            KIMI_HOOK_EVENTS.len()
        );

        // The user's own entry comes first and another table follows ours:
        // uninstall has to take out the middle and leave both ends alone.
        let t = HookTarget::remote(&*host, base.join("sandwich"));
        let config = HookAgent::Kimi.target_path(&t);
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        let user_half = concat!(
            "# mine\n",
            "[[hooks]]\n",
            "event = \"Stop\"\n",
            "command = \"afplay a.aiff\"  # ding\n",
            "\n",
            "[providers.moonshot]\n",
            "api_key = \"secret\"\n",
        );
        std::fs::write(&config, user_half).unwrap();
        install_hooks(&t, HookAgent::Kimi).expect("install");
        assert_eq!(hooks_state(&t, HookAgent::Kimi), HooksState::Installed);
        let merged = std::fs::read_to_string(&config).unwrap();
        assert!(
            merged.starts_with(
                "# mine\n[[hooks]]\nevent = \"Stop\"\ncommand = \"afplay a.aiff\"  # ding\n"
            ),
            "the user's entry and its trailing comment come through verbatim:\n{merged}"
        );
        assert!(merged.contains("[providers.moonshot]\napi_key = \"secret\"\n"));
        assert_eq!(
            uninstall_hooks(&t, HookAgent::Kimi).unwrap(),
            HookOutcome::Removed
        );
        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            user_half,
            "uninstall puts the file back exactly as the user left it"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// Kimi's `Stop` does not fire when the user interrupts a turn or when one
    /// dies on an error, so the pane would sit on "working" forever without
    /// the two events that do.
    #[test]
    fn kimi_reports_every_way_a_turn_ends() {
        for event in ["Stop", "Interrupt", "StopFailure"] {
            assert_eq!(
                KIMI_HOOK_EVENTS
                    .iter()
                    .find(|(hook_event, _)| *hook_event == event)
                    .map(|(_, tty7_event)| *tty7_event),
                Some("stop"),
                "{event} has to end the turn like Stop does"
            );
        }
        assert!(
            !KIMI_HOOK_EVENTS
                .iter()
                .any(|(hook_event, _)| *hook_event == "Notification"),
            "Notification fires for background-task chatter and would strand the pane on waiting"
        );
    }
}
