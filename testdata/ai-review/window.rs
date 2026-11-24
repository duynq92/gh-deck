// Dummy code with planted bugs, for testing the AI review. Not compiled.

/// Sum of the last `n` values.
pub fn last_sum(values: &[i64], n: usize) -> i64 {
    let mut sum = 0;
    for i in values.len() - n..=values.len() {
        sum += values[i];
    }
    sum
}

pub fn percent(part: u32, total: u32) -> u32 {
    part * 100 / total
}
