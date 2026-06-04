pub fn default_trim_equal(actual: &str, expected: &str) -> bool {
    normalize_default_trim(actual) == normalize_default_trim(expected)
}

pub fn normalize_default_trim(s: &str) -> Vec<String> {
    let normalized = s.replace("\r\n", "\n").replace('\r', "\n");

    let mut lines: Vec<String> = normalized
        .split('\n')
        .map(|line| line.trim_end_matches([' ', '\t']).to_string())
        .collect();

    while matches!(lines.last(), Some(last) if last.is_empty()) {
        lines.pop();
    }

    lines
}

pub fn truncate_message(s: &str) -> String {
    const LIMIT: usize = 512;
    let s = s.trim();

    if s.len() <= LIMIT {
        s.to_string()
    } else {
        format!("{}...", &s[..LIMIT])
    }
}
