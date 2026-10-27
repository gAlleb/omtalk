//! HTTP(S) links and their display-column hitboxes, including wrapped links.
use unicode_width::UnicodeWidthStr;

pub type Hit = (usize, usize, String);

pub fn urls(text: &str) -> Vec<String> {
    let lower = text.to_ascii_lowercase();
    let mut at = 0;
    let mut found = Vec::new();
    while at < text.len() {
        let Some(offset) = ["https://", "http://"]
            .iter()
            .filter_map(|prefix| lower[at..].find(prefix))
            .min()
        else {
            break;
        };
        let start = at + offset;
        let end = text[start..]
            .find(|c: char| {
                c.is_whitespace() || c.is_control() || matches!(c, '<' | '>' | '"' | '\'' | '`' | '«' | '»')
            })
            .map_or(text.len(), |n| start + n);
        let mut url = text[start..end].trim_end_matches(['.', ',', '!', '?', ';', ':']);
        loop {
            let pair = match url.chars().last() {
                Some(')') => Some(('(', ')')),
                Some(']') => Some(('[', ']')),
                Some('}') => Some(('{', '}')),
                _ => None,
            };
            let Some((open, close)) = pair else { break };
            if url.matches(close).count() <= url.matches(open).count() {
                break;
            }
            url = &url[..url.len() - close.len_utf8()];
        }
        if url
            .split_once("://")
            .is_some_and(|(_, host)| !host.is_empty() && !host.starts_with('/'))
        {
            found.push(url.to_owned());
        }
        at = end.max(start + 1);
    }
    found.sort_by_key(|u| std::cmp::Reverse(u.len()));
    found.dedup();
    found
}

/// Join rendered rows without wrapping indentation. Each remaining character keeps
/// its original row/column, so clicking any visible fragment opens the whole URL.
/// Targets come only from the source message, never from surrounding UI text.
pub fn row_hits(source: &str, rows: &[String]) -> Vec<Vec<Hit>> {
    let mut hits: Vec<Vec<Hit>> = vec![Vec::new(); rows.len()];
    let urls = urls(source);
    if urls.is_empty() {
        return hits;
    }
    let mut flat = String::new();
    let mut positions = Vec::new();
    for (row, text) in rows.iter().enumerate() {
        for (byte, c) in text.char_indices() {
            if c.is_whitespace() || c == '>' {
                continue;
            }
            let start = text[..byte].width();
            let end = text[..byte + c.len_utf8()].width();
            positions.push((flat.len(), row, start, end));
            flat.push(c);
        }
    }
    for url in urls {
        for (start, _) in flat.match_indices(url.as_str()) {
            let end = start + url.len();
            for &(_, row, col, to) in positions
                .iter()
                .filter(|(byte, ..)| *byte >= start && *byte < end)
            {
                if to <= col {
                    continue;
                }
                if hits[row]
                    .iter()
                    .any(|(a, b, existing)| *a < to && col < *b && existing != &url)
                {
                    continue;
                }
                if let Some((_, previous_end, previous)) = hits[row].last_mut()
                    && *previous_end == col
                    && previous == &url
                {
                    *previous_end = to;
                } else {
                    hits[row].push((col, to, url.clone()));
                }
            }
        }
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_urls_keep_balanced_paths_and_ignore_sentence_punctuation() {
        assert_eq!(
            urls("(https://example.org/a_(b)), https://example.org/second."),
            ["https://example.org/second", "https://example.org/a_(b)"]
        );
        assert!(urls("aesgcm://secret file:///tmp/file javascript:alert(1)").is_empty());
    }
    #[test]
    fn wrapped_links_keep_the_whole_target_and_correct_unicode_columns() {
        let url = "https://example.org/long/path";
        let rows = vec!["  😀 ссылка https://example.".into(), "    org/long/path".into()];
        let hits = row_hits(url, &rows);
        assert_eq!(hits[0][0].0, "  😀 ссылка ".width());
        assert_eq!(hits[1][0], (4, 17, url.into()));
        assert!(hits.iter().flatten().all(|(_, _, target)| target == url));
    }
}
