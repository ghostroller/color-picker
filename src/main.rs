#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

fn main() -> std::process::ExitCode {
    #[cfg(windows)]
    {
        if let Err(error) = color_picker::platform::windows::run() {
            eprintln!("color-picker: {error}");
            return std::process::ExitCode::FAILURE;
        }
        std::process::ExitCode::SUCCESS
    }

    #[cfg(not(windows))]
    {
        eprintln!("当前仅支持 Windows");
        std::process::ExitCode::FAILURE
    }
}
