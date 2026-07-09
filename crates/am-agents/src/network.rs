/// Detect provider failures that are likely caused by the machine being
/// offline or unable to reach the cloud endpoint. Usage/rate limits are handled
/// separately by `limits`.
pub fn detect_network_error(message: &str) -> Option<String> {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    let patterns = [
        "could not resolve host",
        "dns error",
        "dns lookup",
        "failed to lookup address",
        "temporary failure in name resolution",
        "name or service not known",
        "nodename nor servname provided",
        "network is unreachable",
        "network unreachable",
        "no internet connection",
        "internet connection appears to be offline",
        "offline",
        "connection timed out",
        "request timed out",
        "operation timed out",
        "deadline has elapsed",
        "timed out while trying to connect",
        "connection reset",
        "connection aborted",
        "connection closed before",
        "connection refused",
        "eai_again",
        "enotfound",
        "enetunreach",
        "etimedout",
        "econnreset",
        "econnrefused",
        "tls handshake timeout",
        "failed to connect",
        "error sending request",
        "connection error",
    ];

    patterns
        .iter()
        .any(|pattern| lower.contains(pattern))
        .then(|| summarize(trimmed, 2000))
}

fn summarize(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    let mut out: String = value.chars().take(max).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_dns_and_timeout_errors() {
        assert!(detect_network_error("error sending request: dns error").is_some());
        assert!(detect_network_error("request timed out after 30s").is_some());
        assert!(detect_network_error("ENOTFOUND api.openai.com").is_some());
    }

    #[test]
    fn ignores_usage_and_auth_errors() {
        assert!(detect_network_error("rate limit exceeded").is_none());
        assert!(detect_network_error("401 unauthorized").is_none());
    }
}
