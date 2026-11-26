//! A small, deterministic unified diff used for `--dry-run` previews.

const CONTEXT: usize = 3;
const MAX_CELLS: usize = 4_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Equal(usize, usize),
    Delete(usize),
    Insert(usize),
}

fn lcs_ops(before: &[&str], after: &[&str]) -> Vec<Op> {
    let n = before.len();
    let m = after.len();
    if n.saturating_mul(m) > MAX_CELLS {
        return coarse_ops(before, after);
    }
    let width = m + 1;
    let mut table = vec![0u32; (n + 1) * width];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i * width + j] = if before[i] == after[j] {
                table[(i + 1) * width + j + 1] + 1
            } else {
                table[(i + 1) * width + j].max(table[i * width + j + 1])
            };
        }
    }
    let mut ops = Vec::with_capacity(n + m);
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if before[i] == after[j] {
            ops.push(Op::Equal(i, j));
            i += 1;
            j += 1;
        } else if table[(i + 1) * width + j] >= table[i * width + j + 1] {
            ops.push(Op::Delete(i));
            i += 1;
        } else {
            ops.push(Op::Insert(j));
            j += 1;
        }
    }
    while i < n {
        ops.push(Op::Delete(i));
        i += 1;
    }
    while j < m {
        ops.push(Op::Insert(j));
        j += 1;
    }
    ops
}

fn coarse_ops(before: &[&str], after: &[&str]) -> Vec<Op> {
    let prefix = before
        .iter()
        .zip(after)
        .take_while(|(left, right)| left == right)
        .count();
    let max_suffix = before.len().min(after.len()).saturating_sub(prefix);
    let suffix = before
        .iter()
        .rev()
        .zip(after.iter().rev())
        .take(max_suffix)
        .take_while(|(left, right)| left == right)
        .count();
    let mut ops = Vec::new();
    for index in 0..prefix {
        ops.push(Op::Equal(index, index));
    }
    for index in prefix..before.len() - suffix {
        ops.push(Op::Delete(index));
    }
    for index in prefix..after.len() - suffix {
        ops.push(Op::Insert(index));
    }
    for offset in 0..suffix {
        ops.push(Op::Equal(
            before.len() - suffix + offset,
            after.len() - suffix + offset,
        ));
    }
    ops
}

/// Returns a unified diff, or an empty string when the texts are equal.
pub fn unified_diff(before: &str, after: &str, old_label: &str, new_label: &str) -> String {
    if before == after {
        return String::new();
    }
    let before_lines: Vec<&str> = before.lines().collect();
    let after_lines: Vec<&str> = after.lines().collect();
    let ops = lcs_ops(&before_lines, &after_lines);

    // Group changes that are separated by at most 2 * CONTEXT equal lines.
    let mut groups: Vec<(usize, usize)> = Vec::new();
    let mut index = 0;
    while index < ops.len() {
        let is_change = !matches!(ops[index], Op::Equal(_, _));
        if !is_change {
            index += 1;
            continue;
        }
        let start = index;
        let mut end = index;
        let mut cursor = index + 1;
        while cursor < ops.len() {
            if matches!(ops[cursor], Op::Equal(_, _)) {
                let mut run = cursor;
                while run < ops.len() && matches!(ops[run], Op::Equal(_, _)) {
                    run += 1;
                }
                if run - cursor > 2 * CONTEXT || run == ops.len() {
                    break;
                }
                cursor = run;
                continue;
            }
            end = cursor;
            cursor += 1;
        }
        groups.push((start, end));
        index = end + 1;
    }
    if groups.is_empty() {
        return String::new();
    }

    let mut out = String::new();
    out.push_str(&format!("--- {old_label}\n"));
    out.push_str(&format!("+++ {new_label}\n"));
    let mut old_consumed = 0;
    let mut new_consumed = 0;
    let mut op_index = 0;

    for (start, end) in groups {
        // Advance counters to the first context line of this hunk.
        let hunk_start = start.saturating_sub(CONTEXT);
        while op_index < hunk_start {
            match ops[op_index] {
                Op::Equal(_, _) => {
                    old_consumed += 1;
                    new_consumed += 1;
                }
                Op::Delete(_) => old_consumed += 1,
                Op::Insert(_) => new_consumed += 1,
            }
            op_index += 1;
        }
        let hunk_end = (end + CONTEXT + 1).min(ops.len());
        let old_start = old_consumed;
        let new_start = new_consumed;
        let mut old_count = 0;
        let mut new_count = 0;
        for op in &ops[hunk_start..hunk_end] {
            match op {
                Op::Equal(_, _) => {
                    old_count += 1;
                    new_count += 1;
                }
                Op::Delete(_) => old_count += 1,
                Op::Insert(_) => new_count += 1,
            }
        }
        out.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            if old_count == 0 {
                old_start
            } else {
                old_start + 1
            },
            old_count,
            if new_count == 0 {
                new_start
            } else {
                new_start + 1
            },
            new_count
        ));
        for op in &ops[hunk_start..hunk_end] {
            match *op {
                Op::Equal(old, _) => out.push_str(&format!(" {}\n", before_lines[old])),
                Op::Delete(old) => out.push_str(&format!("-{}\n", before_lines[old])),
                Op::Insert(new) => out.push_str(&format!("+{}\n", after_lines[new])),
            }
        }
        while op_index < hunk_end {
            match ops[op_index] {
                Op::Equal(_, _) => {
                    old_consumed += 1;
                    new_consumed += 1;
                }
                Op::Delete(_) => old_consumed += 1,
                Op::Insert(_) => new_consumed += 1,
            }
            op_index += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_text_has_no_diff() {
        assert_eq!(unified_diff("a\n", "a\n", "a", "b"), "");
    }

    #[test]
    fn renders_a_deterministic_hunk() {
        let diff = unified_diff("a\nb\nc\n", "a\nB\nc\n", "old", "new");
        assert_eq!(diff, "--- old\n+++ new\n@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n");
    }

    #[test]
    fn handles_insertions_and_removals() {
        let diff = unified_diff("a\nc\n", "a\nb\nc\n", "old", "new");
        assert!(diff.contains("+b\n"));
        let diff = unified_diff("a\nb\nc\n", "a\nc\n", "old", "new");
        assert!(diff.contains("-b\n"));
    }
}
