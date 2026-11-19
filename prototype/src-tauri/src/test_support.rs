//! Shared helpers for tests that run the real Python contest host.

/// The Python interpreter for host-backed tests: `XLOG_TEST_PYTHON` when set,
/// otherwise the first of `python3.12`, `python3` or `python` that reports
/// Python 3.12 or newer. Windows installs usually provide only `python`.
pub(crate) fn python() -> String {
    if let Ok(explicit) = std::env::var("XLOG_TEST_PYTHON") {
        return explicit;
    }
    for candidate in ["python3.12", "python3", "python"] {
        let Ok(output) = std::process::Command::new(candidate).arg("--version").output() else { continue };
        if !output.status.success() { continue; }
        let text = String::from_utf8_lossy(if output.stdout.is_empty() { &output.stderr } else { &output.stdout }).to_string();
        let mut parts = text.trim().trim_start_matches("Python ").split('.');
        let major = parts.next().and_then(|part| part.parse::<u32>().ok());
        let minor = parts.next().and_then(|part| part.parse::<u32>().ok());
        if matches!((major, minor), (Some(3), Some(minor)) if minor >= 12) {
            return candidate.into();
        }
    }
    panic!("Host-backed tests need Python 3.12 or newer; set XLOG_TEST_PYTHON to its path.")
}
