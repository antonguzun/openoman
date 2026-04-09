use std::sync::OnceLock;

use tracing_subscriber::{fmt, EnvFilter};

static LOGGING: OnceLock<()> = OnceLock::new();

pub(crate) fn init() {
    LOGGING.get_or_init(|| {
        let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
        let _ = fmt().with_env_filter(filter).with_target(true).try_init();
    });
}
