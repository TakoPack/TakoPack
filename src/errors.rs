use anyhow;

pub type Result<T> = std::result::Result<T, anyhow::Error>;
pub use anyhow::format_err;

#[macro_export]
macro_rules! takopack_info {
    ($e:expr) => {
        {
            use nu_ansi_term::Color::Green;
            eprintln!("{}", Green.paint($e));
        }
    };

    ($fmt:expr, $( $arg:tt)+) => {
        {
            use nu_ansi_term::Color::Green;
            let print_string = format!($fmt, $($arg)+);
            eprintln!("{}", Green.paint(print_string));
        }
    };
}

#[macro_export]
macro_rules! takopack_warn {
    ($e:expr) => {
        {
            use nu_ansi_term::Color::Rgb;
            eprintln!("{}", Rgb(255,165,0).bold().paint($e));
        }
    };

    ($fmt:expr, $( $arg:tt)+) => {
        {
            use nu_ansi_term::Color::Rgb;
            let print_string = Rgb(255,165,0).bold().paint(format!($fmt, $($arg)+));
            eprintln!("{}", print_string);
        }
    };

}

#[macro_export]
macro_rules! takopack_bail {
    ($e:expr) => {{
        return Err(::anyhow::format_err!("{}", $e));
    }};

    ($fmt:expr, $( $arg:tt)+) => {
        {
            let error_string = format!($fmt, $($arg)+);
            return Err(::anyhow::format_err!("{}", error_string));
        }
    };
}
