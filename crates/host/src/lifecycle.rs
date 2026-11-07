//! Explicit, user-scoped Host service lifecycle.
//!
//! Nothing here runs from `serve`: installation is only ever an explicit
//! operator command. The service runs as the interactive user (launchd user
//! agent, systemd `--user`, or a Windows logon Scheduled Task), never as root,
//! LocalSystem or an elevated global service.
//!
//! Starting, restarting, stopping or uninstalling the Host never signals
//! herdr-core or the Agents it owns. The service manager only owns the Host
//! process, which is a metadata/RPC endpoint.

use super::*;
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(any(test, target_os = "macos"))]
const PLIST_TEMPLATE: &str = include_str!("../service/com.ukk.agent-host.plist");
#[cfg(any(test, target_os = "linux"))]
const SYSTEMD_TEMPLATE: &str = include_str!("../service/ukk-agent-host.service");
#[cfg(any(test, windows))]
const WINDOWS_TEMPLATE: &str = include_str!("../service/windows/ukk-agent-host.ps1");

fn executable_path() -> AppResult<PathBuf> {
    let executable = env::current_exe()?;
    #[cfg(unix)]
    {
        Ok(executable.canonicalize().unwrap_or(executable))
    }
    #[cfg(not(unix))]
    {
        Ok(executable)
    }
}

#[cfg(any(test, target_os = "macos"))]
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(any(test, target_os = "linux"))]
fn systemd_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '%' => escaped.push_str("%%"),
            '$' => escaped.push_str("$$"),
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            _ => escaped.push(character),
        }
    }
    escaped
}

#[cfg(any(test, windows))]
fn powershell_escape(value: &str) -> String {
    value
        .replace('`', "``")
        .replace('$', "`$")
        .replace('"', "`\"")
}

/// Pure renderers are exercised by tests on every platform; no service
/// manager is invoked there.
#[cfg(any(test, target_os = "macos"))]
fn render_launchd_plist(executable: &Path, log: &Path) -> String {
    PLIST_TEMPLATE
        .replace("{{EXE}}", &xml_escape(&executable.to_string_lossy()))
        .replace("{{LOG}}", &xml_escape(&log.to_string_lossy()))
}

#[cfg(any(test, target_os = "linux"))]
fn render_systemd_unit(executable: &Path) -> String {
    SYSTEMD_TEMPLATE.replace("{{EXE}}", &systemd_escape(&executable.to_string_lossy()))
}

#[cfg(any(test, windows))]
fn render_windows_script(executable: &Path) -> String {
    WINDOWS_TEMPLATE.replace("{{EXE}}", &powershell_escape(&executable.to_string_lossy()))
}

fn run_checked(command: &mut Command, description: &str) -> AppResult<()> {
    let output = command
        .output()
        .map_err(|error| format!("could not run {description}: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let detail = if stderr.trim().is_empty() {
        stdout.trim().to_owned()
    } else {
        stderr.trim().to_owned()
    };
    Err(format!("{description} failed ({}): {detail}", output.status).into())
}

#[cfg(unix)]
fn print_unix_endpoint() -> AppResult<()> {
    let path = transport::socket_path()?;
    println!("  endpoint: {}", path.display());
    for line in local_fs::describe_endpoint(&path) {
        println!("{line}");
    }
    Ok(())
}

// ---------------------------------------------------------------- macOS ----

#[cfg(target_os = "macos")]
const LAUNCHD_LABEL: &str = "com.ukk.agent-host";

#[cfg(target_os = "macos")]
fn user_id() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

#[cfg(target_os = "macos")]
fn home_directory() -> AppResult<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is required for user-scoped service paths".into())
}

#[cfg(target_os = "macos")]
fn launchd_plist_path() -> AppResult<PathBuf> {
    Ok(home_directory()?
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{LAUNCHD_LABEL}.plist")))
}

#[cfg(target_os = "macos")]
fn launchd_domain() -> String {
    format!("gui/{}", user_id())
}

#[cfg(target_os = "macos")]
pub(super) fn install() -> AppResult<()> {
    let executable = executable_path()?;
    let plist = launchd_plist_path()?;
    let log = home_directory()?
        .join("Library")
        .join("Logs")
        .join("ukk-agent-host.log");
    if let Some(parent) = plist.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if let Some(parent) = log.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&plist, render_launchd_plist(&executable, &log))?;
    let domain = launchd_domain();
    let service = format!("{domain}/{LAUNCHD_LABEL}");
    // Drop an older registration first so program-path and template changes
    // take effect. A failure here usually means "not loaded"; bootstrap below
    // produces the actionable diagnostic.
    let _ = Command::new("launchctl")
        .arg("bootout")
        .arg(&service)
        .status();
    run_checked(
        Command::new("launchctl")
            .args(["bootstrap", &domain])
            .arg(&plist),
        "launchctl bootstrap",
    )?;
    let _ = Command::new("launchctl")
        .arg("enable")
        .arg(&service)
        .status();
    println!("Installed launchd user agent {LAUNCHD_LABEL}.");
    println!("  plist: {plist}", plist = plist.display());
    println!("  log:   {log}", log = log.display());
    println!(
        "The Host restarts independently of Desktop windows. herdr-core Agents are not signalled."
    );
    Ok(())
}

#[cfg(target_os = "macos")]
pub(super) fn uninstall() -> AppResult<()> {
    let plist = launchd_plist_path()?;
    let service = format!("{}/{LAUNCHD_LABEL}", launchd_domain());
    let _ = Command::new("launchctl")
        .arg("bootout")
        .arg(&service)
        .status();
    match std::fs::remove_file(&plist) {
        Ok(()) => println!("Removed {}", plist.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            println!(
                "No launchd plist at {}; nothing to remove.",
                plist.display()
            )
        }
        Err(error) => return Err(error.into()),
    }
    println!("ukk Host service removed. herdr-core Agents were not signalled and keep running.");
    Ok(())
}

#[cfg(target_os = "macos")]
pub(super) fn restart() -> AppResult<()> {
    let service = format!("{}/{LAUNCHD_LABEL}", launchd_domain());
    run_checked(
        Command::new("launchctl")
            .args(["kickstart", "-k"])
            .arg(&service),
        "launchctl kickstart",
    )?;
    println!("Restarted {LAUNCHD_LABEL}. herdr-core Agents were not signalled.");
    Ok(())
}

#[cfg(target_os = "macos")]
pub(super) fn status() -> AppResult<()> {
    let plist = launchd_plist_path()?;
    let service = format!("{}/{LAUNCHD_LABEL}", launchd_domain());
    println!("ukk Host service: macOS launchd user agent");
    println!(
        "  plist: {} ({})",
        plist.display(),
        if plist.exists() {
            "present"
        } else {
            "not installed"
        }
    );
    let loaded = Command::new("launchctl")
        .arg("print")
        .arg(&service)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);
    println!(
        "  launchd job: {}",
        if loaded {
            "loaded"
        } else {
            "not loaded (run `ukk-host install`)"
        }
    );
    print_unix_endpoint()
}

// ---------------------------------------------------------------- Linux ----

#[cfg(target_os = "linux")]
const SYSTEMD_UNIT: &str = "ukk-agent-host.service";

#[cfg(target_os = "linux")]
fn systemd_unit_path() -> AppResult<PathBuf> {
    let config = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .ok_or("HOME or XDG_CONFIG_HOME is required for the user unit")?;
    Ok(config.join("systemd").join("user").join(SYSTEMD_UNIT))
}

#[cfg(target_os = "linux")]
pub(super) fn install() -> AppResult<()> {
    let executable = executable_path()?;
    let unit = systemd_unit_path()?;
    if let Some(parent) = unit.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&unit, render_systemd_unit(&executable))?;
    run_checked(
        Command::new("systemctl").args(["--user", "daemon-reload"]),
        "systemctl --user daemon-reload",
    )?;
    run_checked(
        Command::new("systemctl").args(["--user", "enable", "--now", SYSTEMD_UNIT]),
        "systemctl --user enable --now",
    )?;
    println!("Installed systemd user unit {SYSTEMD_UNIT}.");
    println!("  unit: {unit}", unit = unit.display());
    println!(
        "The Host restarts independently of Desktop windows. herdr-core Agents are not signalled."
    );
    Ok(())
}

#[cfg(target_os = "linux")]
pub(super) fn uninstall() -> AppResult<()> {
    let unit = systemd_unit_path()?;
    let _ = Command::new("systemctl")
        .args(["--user", "disable", "--now", SYSTEMD_UNIT])
        .status();
    match std::fs::remove_file(&unit) {
        Ok(()) => println!("Removed {}", unit.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            println!("No user unit at {}; nothing to remove.", unit.display())
        }
        Err(error) => return Err(error.into()),
    }
    let _ = Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status();
    println!("ukk Host service removed. herdr-core Agents were not signalled and keep running.");
    Ok(())
}

#[cfg(target_os = "linux")]
pub(super) fn restart() -> AppResult<()> {
    run_checked(
        Command::new("systemctl").args(["--user", "restart", SYSTEMD_UNIT]),
        "systemctl --user restart",
    )?;
    println!("Restarted {SYSTEMD_UNIT}. herdr-core Agents were not signalled.");
    Ok(())
}

#[cfg(target_os = "linux")]
pub(super) fn status() -> AppResult<()> {
    let unit = systemd_unit_path()?;
    println!("ukk Host service: systemd --user unit");
    println!(
        "  unit: {} ({})",
        unit.display(),
        if unit.exists() {
            "present"
        } else {
            "not installed"
        }
    );
    for (verb, label) in [("is-active", "active"), ("is-enabled", "enabled")] {
        let state = Command::new("systemctl")
            .args(["--user", verb, SYSTEMD_UNIT])
            .output()
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
            .unwrap_or_else(|error| format!("unavailable ({error})"));
        println!("  {label}: {state}");
    }
    print_unix_endpoint()
}

// -------------------------------------------------------------- Windows ----

#[cfg(windows)]
const WINDOWS_TASK: &str = "ukk-agent-host";

#[cfg(windows)]
fn local_app_data() -> AppResult<PathBuf> {
    env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| {
            env::var_os("USERPROFILE").map(|home| PathBuf::from(home).join("AppData").join("Local"))
        })
        .ok_or_else(|| "LOCALAPPDATA or USERPROFILE is required".into())
}

#[cfg(windows)]
fn task_runs_now() -> AppResult<()> {
    run_checked(
        Command::new("schtasks.exe").args(["/Run", "/TN", WINDOWS_TASK]),
        "schtasks /Run",
    )
}

#[cfg(windows)]
pub(super) fn install() -> AppResult<()> {
    let executable = executable_path()?;
    let directory = local_app_data()?.join("ukk");
    std::fs::create_dir_all(&directory)?;
    let script = directory.join("ukk-agent-host.ps1");
    std::fs::write(&script, render_windows_script(&executable))?;
    run_checked(
        Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
            ])
            .arg(&script),
        "Register-ScheduledTask",
    )?;
    task_runs_now()?;
    println!("Installed Windows logon task {WINDOWS_TASK} for the current user.");
    println!("  script: {script}", script = script.display());
    println!(
        "The task is user-scoped (limited token) and starts at logon, not from a Desktop window."
    );
    println!("herdr-core Agents are not signalled.");
    Ok(())
}

#[cfg(windows)]
pub(super) fn uninstall() -> AppResult<()> {
    let query = Command::new("schtasks.exe")
        .args(["/Query", "/TN", WINDOWS_TASK])
        .output()
        .map_err(|error| format!("could not run schtasks /Query: {error}"))?;
    if !query.status.success() {
        println!("No scheduled task named {WINDOWS_TASK}; nothing to remove.");
        return Ok(());
    }
    // Stopping the scheduled task terminates the Host process only. herdr-core
    // and its Agents are separate processes and are never signalled here.
    let _ = Command::new("schtasks.exe")
        .args(["/End", "/TN", WINDOWS_TASK])
        .status();
    run_checked(
        Command::new("schtasks.exe").args(["/Delete", "/TN", WINDOWS_TASK, "/F"]),
        "schtasks /Delete",
    )?;
    println!("Removed scheduled task {WINDOWS_TASK}. herdr-core Agents were not signalled.");
    Ok(())
}

#[cfg(windows)]
pub(super) fn restart() -> AppResult<()> {
    // `/End` stops the Host endpoint, not the Agent runtime.
    let _ = Command::new("schtasks.exe")
        .args(["/End", "/TN", WINDOWS_TASK])
        .status();
    task_runs_now()?;
    println!("Restarted {WINDOWS_TASK}. herdr-core Agents were not signalled.");
    Ok(())
}

#[cfg(windows)]
pub(super) fn status() -> AppResult<()> {
    println!("ukk Host service: Windows user logon Scheduled Task");
    let query = Command::new("schtasks.exe")
        .args(["/Query", "/TN", WINDOWS_TASK])
        .output();
    match query {
        Ok(output) if output.status.success() => {
            println!("  task: {WINDOWS_TASK} (registered)");
        }
        Ok(_) => println!("  task: {WINDOWS_TASK} (not registered; run `ukk-host install`)"),
        Err(error) => println!("  task: query failed ({error})"),
    }
    match ukk_host_client::windows_pipe::pipe_name() {
        Ok(name) => println!("  endpoint: {name}"),
        Err(error) => println!("  endpoint: unavailable ({error})"),
    }
    match ukk_host_client::windows_pipe::open_client() {
        Ok(_) => println!("  pipe: live same-user Host accepted the connection"),
        Err(error) => println!("  pipe: no same-user Host accepted the connection ({error})"),
    }
    Ok(())
}

// -------------------------------------------------- other Unix / fallback ---

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
pub(super) fn install() -> AppResult<()> {
    Err("Host service installation is supported on macOS, Linux and Windows only".into())
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
pub(super) fn uninstall() -> AppResult<()> {
    Err("Host service installation is supported on macOS, Linux and Windows only".into())
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
pub(super) fn restart() -> AppResult<()> {
    Err("Host service installation is supported on macOS, Linux and Windows only".into())
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
pub(super) fn status() -> AppResult<()> {
    Err("Host service status is supported on macOS, Linux and Windows only".into())
}

// --------------------------------------------------------------- cleanup ---

pub(super) fn cleanup() -> AppResult<()> {
    #[cfg(unix)]
    {
        let path = transport::socket_path()?;
        let message = local_fs::cleanup_stale_socket(&path)
            .map_err(|error| -> Box<dyn Error + Send + Sync> { error.into() })?;
        println!("{message}");
    }
    #[cfg(windows)]
    {
        println!(
            "Windows Named Pipe names disappear with the last handle; \
             there is no stale socket file to remove."
        );
    }
    #[cfg(not(any(unix, windows)))]
    {
        return Err("Host endpoint cleanup is supported on macOS, Linux and Windows only".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_executable() -> &'static Path {
        Path::new("/opt/ukk & tools/bin/ukk-host")
    }

    #[test]
    fn launchd_template_is_a_user_agent_that_restarts_independently() {
        let rendered = render_launchd_plist(sample_executable(), Path::new("/tmp/ukk.log"));
        assert!(rendered.contains("<string>com.ukk.agent-host</string>"));
        assert!(rendered.contains("<key>RunAtLoad</key>"));
        assert!(rendered.contains("<key>KeepAlive</key>"));
        assert!(rendered.contains("<string>serve</string>"));
        assert!(rendered.contains("/opt/ukk &amp; tools/bin/ukk-host"));
        assert!(!rendered.contains("{{EXE}}"));
        // launchd user agents live in ~/Library/LaunchAgents; never a
        // root-level LaunchDaemon or a UserName key.
        assert!(!rendered.contains("LaunchDaemons"));
        assert!(!rendered.contains("<key>UserName</key>"));
    }

    #[test]
    fn systemd_template_is_a_user_unit_and_spares_agents() {
        let rendered = render_systemd_unit(sample_executable());
        assert!(rendered.contains("ExecStart=\"/opt/ukk & tools/bin/ukk-host\" serve"));
        assert!(rendered.contains("WantedBy=default.target"));
        // KillMode=process keeps a service stop scoped to the Host process.
        assert!(rendered.contains("KillMode=process"));
        assert!(rendered.contains("Restart=on-failure"));
        assert!(!rendered.contains("{{EXE}}"));
        assert!(!rendered.contains("User="));
        assert!(!rendered.contains("WantedBy=multi-user.target"));
    }

    #[test]
    fn systemd_template_escapes_specifiers_and_variables() {
        let rendered = render_systemd_unit(Path::new("/opt/100%/$HOME/ukk-host"));
        assert!(rendered.contains("ExecStart=\"/opt/100%%/$$HOME/ukk-host\" serve"));
    }

    #[test]
    fn windows_template_is_a_limited_user_logon_task() {
        let rendered = render_windows_script(Path::new(r"C:\Program Files\ukk\ukk-host.exe"));
        assert!(rendered.contains("Register-ScheduledTask"));
        assert!(rendered.contains("-LogonType Interactive"));
        assert!(rendered.contains("-RunLevel Limited"));
        assert!(rendered.contains("-AtLogOn"));
        assert!(rendered.contains("ukk-agent-host"));
        assert!(!rendered.contains("{{EXE}}"));
        // Never a LocalSystem/global service.
        assert!(!rendered.contains("LocalSystem"));
        assert!(!rendered.contains("New-Service"));
    }

    #[test]
    fn windows_template_escapes_powershell_expansion() {
        let rendered = render_windows_script(Path::new(r"C:\a ` $HOME\ukk-host.exe"));
        assert!(rendered.contains(r#"C:\a `` `$HOME\ukk-host.exe"#));
    }
}
