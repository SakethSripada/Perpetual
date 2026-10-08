/// `CREATE_NO_WINDOW`: run a console program without allocating a console.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Keeps background helper processes (git, provider CLIs, runtime probes) from
/// flashing a console window when spawned by the GUI-subsystem desktop app.
/// No-op on other platforms.
pub fn hide_console(command: &mut std::process::Command) -> &mut std::process::Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}
