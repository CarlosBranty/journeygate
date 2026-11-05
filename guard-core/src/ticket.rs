//! Ticket key extraction from branch names (`[tracker].ticket_pattern`).

pub use regex::Regex;

#[cfg(test)]
mod tests;

pub fn compile_pattern(pattern: &str) -> Result<Regex, regex::Error> {
    Regex::new(pattern)
}

/// The first match of `pattern` in the branch name. A detached `HEAD`
/// (reported by git as the literal `HEAD`) has no ticket.
pub fn extract_ticket(pattern: &Regex, branch: Option<&str>) -> Option<String> {
    let branch = branch.filter(|b| *b != "HEAD")?;
    pattern.find(branch).map(|m| m.as_str().to_string())
}

/// `@ticket:<KEY>`.
pub fn ticket_tag(key: &str) -> String {
    format!("@ticket:{key}")
}
