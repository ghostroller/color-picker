#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

fn main() -> std::process::ExitCode {
    #[cfg(windows)]
    {
        use color_picker::app::{
            cli::{Options, USAGE},
            diagnostics,
        };
        let options = match Options::parse(std::env::args_os().skip(1)) {
            Ok(options) => options,
            Err(error) => {
                eprintln!("{error}\n{USAGE}");
                return std::process::ExitCode::from(2);
            }
        };
        if let Some(path) = &options.log_file
            && let Err(error) = diagnostics::init(path)
        {
            let message = format!("无法打开诊断日志 {}：{error}", path.display());
            eprintln!("{message}");
            if !options.check_environment && !options.quit {
                color_picker::platform::windows::host::show_error(&message);
            }
            return std::process::ExitCode::FAILURE;
        }
        diagnostics::event(format_args!(
            "app.start version={} check_environment={} diagnostics={} startup={} quit={}",
            env!("CARGO_PKG_VERSION"),
            options.check_environment,
            options.diagnostics,
            options.startup,
            options.quit,
        ));
        // Installer control never creates resident resources or checks display
        // requirements: it only asks this installation's existing process to exit.
        let result = if options.quit {
            color_picker::platform::windows::host::quit_current_installation()
        } else {
            color_picker::platform::windows::check_environment().and_then(|()| {
                diagnostics::event(format_args!("environment.pmv2_ok"));
                if options.check_environment {
                    println!("color-picker: PerMonitorV2 active");
                    Ok(())
                } else {
                    color_picker::platform::windows::host::run_with_startup(
                        options.diagnostics,
                        options.startup,
                    )
                }
            })
        };
        if let Err(error) = result {
            eprintln!("color-picker: {error}");
            diagnostics::event(format_args!("app.error error={error}"));
            if !options.check_environment && !options.quit {
                color_picker::platform::windows::host::show_error(&error.to_string());
            }
            return std::process::ExitCode::FAILURE;
        }
        diagnostics::event(format_args!("app.exit status=success"));
        std::process::ExitCode::SUCCESS
    }

    #[cfg(not(windows))]
    {
        eprintln!("当前仅支持 Windows");
        std::process::ExitCode::FAILURE
    }
}
