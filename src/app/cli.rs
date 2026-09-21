use std::{ffi::OsString, path::PathBuf};

pub const USAGE: &str = "Usage: color-picker [--startup | --quit | --check-environment] [--diagnostics] [--log-file <path>] [--no-onboarding]";

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Options {
    pub check_environment: bool,
    pub startup: bool,
    pub quit: bool,
    pub diagnostics: bool,
    pub no_onboarding: bool,
    pub log_file: Option<PathBuf>,
}

impl Options {
    pub fn parse(arguments: impl IntoIterator<Item = OsString>) -> Result<Self, String> {
        let mut options = Self::default();
        let mut arguments = arguments.into_iter();
        while let Some(argument) = arguments.next() {
            if argument == "--check-environment" {
                options.check_environment = true;
            } else if argument == "--startup" {
                options.startup = true;
            } else if argument == "--quit" {
                options.quit = true;
            } else if argument == "--diagnostics" {
                options.diagnostics = true;
            } else if argument == "--no-onboarding" {
                options.no_onboarding = true;
            } else if argument == "--log-file" {
                if options.log_file.is_some() {
                    return Err("--log-file must only be specified once".into());
                }
                let path = arguments.next().ok_or("--log-file requires a file path")?;
                if path.is_empty() || path.to_string_lossy().starts_with("--") {
                    return Err("--log-file requires a file path".into());
                }
                options.log_file = Some(path.into());
                options.diagnostics = true;
            } else {
                return Err(format!("Unknown argument: {}", argument.to_string_lossy()));
            }
        }
        if u8::from(options.startup) + u8::from(options.quit) + u8::from(options.check_environment)
            > 1
        {
            return Err("--startup, --quit and --check-environment cannot be combined".into());
        }
        Ok(options)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(arguments: &[&str]) -> Result<Options, String> {
        Options::parse(arguments.iter().map(OsString::from))
    }

    #[test]
    fn logging_is_opt_in_and_implies_diagnostics() {
        assert_eq!(parse(&[]).unwrap(), Options::default());
        assert_eq!(parse(&["--diagnostics"]).unwrap().log_file, None);
        let options =
            parse(&["--log-file", "日志 folder/test.log", "--check-environment"]).unwrap();
        assert!(options.diagnostics && options.check_environment);
        assert_eq!(
            options.log_file,
            Some(PathBuf::from("日志 folder/test.log"))
        );
    }

    #[test]
    fn invalid_arguments_do_not_silently_start_a_resident_process() {
        for arguments in [
            vec!["--log-file"],
            vec!["--log-file", "--diagnostics"],
            vec!["--log-file", ""],
            vec!["--log-file", "a", "--log-file", "b"],
            vec!["--unknown"],
            vec!["--startup", "--quit"],
            vec!["--startup", "--check-environment"],
            vec!["--quit", "--check-environment"],
        ] {
            assert!(parse(&arguments).is_err());
        }
    }

    #[test]
    fn automatic_start_and_quit_are_explicit_separate_modes() {
        assert!(parse(&["--startup"]).unwrap().startup);
        let quit = parse(&["--quit"]).unwrap();
        assert!(quit.quit);
        assert!(!quit.startup);
        assert!(!quit.check_environment);
        assert!(!quit.diagnostics);
        assert!(quit.log_file.is_none());
        let automation = parse(&["--no-onboarding", "--diagnostics"]).unwrap();
        assert!(automation.no_onboarding && automation.diagnostics);
        assert!(!automation.startup);
    }
}
