use log::LevelFilter;
use simplelog::{ColorChoice, ConfigBuilder, TermLogger, TerminalMode};

pub fn init() {
    TermLogger::init(
        LevelFilter::Info,
        ConfigBuilder::new()
            .add_filter_allow_str("groqddbot")
            .build(),
        TerminalMode::Stdout,
        ColorChoice::Auto,
    )
    .unwrap();
}
