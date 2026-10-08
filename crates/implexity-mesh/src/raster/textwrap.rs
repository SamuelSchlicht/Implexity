// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


const WS: [char; 6] = ['\t', '\n', '\u{b}', '\u{c}', '\r', ' '];

#[must_use]
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let expanded = expand_tabs(text, 8);
    let munged: String = expanded.chars().map(|c| if WS.contains(&c) { ' ' } else { c }).collect();

    let mut chunks: Vec<Vec<char>> = Vec::new();
    for c in munged.chars() {
        let is_ws = c == ' ';
        match chunks.last_mut() {
            Some(last) if (last[0] == ' ') == is_ws => last.push(c),
            _ => chunks.push(vec![c]),
        }
    }
    chunks.reverse();
    let mut lines = Vec::new();
    let is_blank = |c: &Vec<char>| c.iter().all(|ch| ch.is_whitespace());
    while !chunks.is_empty() {
        let mut cur: Vec<Vec<char>> = Vec::new();
        let mut cur_len = 0usize;
        if chunks.last().is_some_and(is_blank) && !lines.is_empty() {
            chunks.pop();
        }
        while let Some(last) = chunks.last() {
            let l = last.len();
            if cur_len + l <= width {
                cur_len += l;
                if let Some(c) = chunks.pop() {
                    cur.push(c);
                }
            } else {
                break;
            }
        }
        if chunks.last().is_some_and(|c| c.len() > width) {
            let space_left = if width < 1 { 1 } else { width - cur_len };
            if let Some(last) = chunks.last_mut() {
                let head: Vec<char> = last[..space_left.min(last.len())].to_vec();
                let tail: Vec<char> = last[space_left.min(last.len())..].to_vec();
                cur.push(head);
                *last = tail;
            }
        }
        if cur.last().is_some_and(is_blank) {
            cur.pop();
        }
        if !cur.is_empty() {
            lines.push(cur.iter().flatten().collect());
        }
    }
    lines
}

fn expand_tabs(text: &str, tabsize: usize) -> String {
    let mut out = String::new();
    let mut col = 0usize;
    for c in text.chars() {
        match c {
            '\t' => {
                let n = tabsize - (col % tabsize);
                out.extend(std::iter::repeat_n(' ', n));
                col += n;
            }
            '\n' | '\r' => {
                out.push(c);
                col = 0;
            }
            _ => {
                out.push(c);
                col += 1;
            }
        }
    }
    out
}

