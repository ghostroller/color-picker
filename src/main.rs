#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

fn main() -> std::process::ExitCode {
    #[cfg(windows)]
    {
        let arguments: Vec<_> = std::env::args_os().skip(1).collect();
        let check = arguments.as_slice() == ["--check-environment"];
        let diagnostics = arguments.as_slice() == ["--diagnostics"];
        if !arguments.is_empty() && !check && !diagnostics {
            eprintln!("Usage: color-picker [--check-environment | --diagnostics]");
            return std::process::ExitCode::from(2);
        }
        let result = color_picker::platform::windows::check_environment().and_then(|()| {
            if check {
                println!("color-picker: PerMonitorV2 active");
                Ok(())
            } else {
                color_picker::platform::windows::host::run(diagnostics)
            }
        });
        if let Err(error) = result {
            eprintln!("color-picker: {error}");
            if !check {
                color_picker::platform::windows::host::show_error(&error.to_string());
            }
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
