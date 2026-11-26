//! The variables a line sets, and the words they stand for.

use crate::core::reading::RUNS;

use super::paths::{assignment, is_name};
use super::tables::KEYWORDS;

/// The variables the line has set: each name with the values it may hold.
/// Whether one is exported is not told apart: a `$name` is put in wherever
/// the line spells it, in a string another shell runs too, so `f=x; sh -c
/// 'rm $f'` is read as removing `x`. Seeing more than runs is the safe way
/// to be wrong.
#[derive(Debug, Clone, Default)]
pub(super) struct Variables(Vec<(String, Vec<String>)>);

impl Variables {
    fn values(&self, name: &str) -> Option<&[String]> {
        self.0
            .iter()
            .find(|(known, _)| known == name)
            .map(|(_, values)| values.as_slice())
    }

    /// Sets `name` (with a `+`, appends to it) to each of the values `value`
    /// may stand for.
    fn set(&mut self, name: &str, value: &str) {
        let values = self.substituted(value);
        let (name, values) = match name.strip_suffix('+') {
            Some(name) => {
                let before = self.values(name).unwrap_or(&[]).to_vec();
                let before = if before.is_empty() {
                    vec![String::new()]
                } else {
                    before
                };
                let joined = before
                    .iter()
                    .flat_map(|old| values.iter().map(move |new| format!("{old}{new}")))
                    .take(RUNS)
                    .collect();
                (name, joined)
            }
            None => (name, values),
        };
        self.put(name, values);
    }

    fn put(&mut self, name: &str, values: Vec<String>) {
        match self.0.iter_mut().find(|(known, _)| known == name) {
            Some(known) => known.1 = values,
            None => self.0.push((name.to_string(), values)),
        }
    }

    /// `word` with each `$NAME` and `${NAME}` the line set put in: one text
    /// for each value they may hold, as many as [`RUNS`]. Any other
    /// variable stays as written.
    pub(super) fn substituted(&self, word: &str) -> Vec<String> {
        if self.0.is_empty() || !word.contains('$') {
            return vec![word.to_string()];
        }
        let mut done = vec![String::new()];
        let mut rest = word;
        while let Some(at) = rest.find('$') {
            for text in &mut done {
                text.push_str(&rest[..at]);
            }
            let after = &rest[at + 1..];
            let (name, used) = match after.strip_prefix('{') {
                Some(inner) => match inner.find('}') {
                    Some(end) => (&inner[..end], end + 2),
                    None => ("", 0),
                },
                None => {
                    let end = after
                        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                        .unwrap_or(after.len());
                    (&after[..end], end)
                }
            };
            match self.values(name) {
                Some(values) => {
                    done = done
                        .iter()
                        .flat_map(|text| values.iter().map(move |value| format!("{text}{value}")))
                        .take(RUNS)
                        .collect();
                    rest = &after[used..];
                }
                None => {
                    for text in &mut done {
                        text.push('$');
                    }
                    rest = after;
                }
            }
        }
        for text in &mut done {
            text.push_str(rest);
        }
        done
    }

    /// The words `word` may run as: itself, or each text it stands for,
    /// and that text split at whitespace too when a value put whitespace in
    /// it, as an unquoted variable is split.
    fn choices(&self, word: &str) -> Vec<Vec<String>> {
        let mut choices = Vec::new();
        for text in self.substituted(word) {
            if text != word && text.contains(char::is_whitespace) {
                choices.push(text.split_whitespace().map(str::to_string).collect());
            }
            choices.push(vec![text]);
        }
        choices
    }

    /// The simple commands `words` may run as: every combination of the
    /// choices of its words, as many as [`RUNS`]; past them, runs in which
    /// each choice of each word is taken once.
    pub(super) fn runs(&self, words: &[String]) -> Vec<Vec<String>> {
        let choices: Vec<Vec<Vec<String>>> = words.iter().map(|word| self.choices(word)).collect();
        let combinations = choices
            .iter()
            .try_fold(1usize, |count, choice| count.checked_mul(choice.len()))
            .filter(|count| *count <= RUNS);
        if combinations.is_none() {
            let most = choices.iter().map(Vec::len).max().unwrap_or(1);
            return (0..most)
                .map(|at| {
                    choices
                        .iter()
                        .flat_map(|choice| choice[at % choice.len()].clone())
                        .collect()
                })
                .collect();
        }
        let mut runs: Vec<Vec<String>> = vec![Vec::new()];
        for choice in &choices {
            runs = runs
                .iter()
                .flat_map(|run| {
                    choice.iter().map(move |words| {
                        let mut run = run.clone();
                        run.extend(words.iter().cloned());
                        run
                    })
                })
                .collect();
        }
        runs
    }

    /// Records what the simple command `words` sets: `NAME=value` alone, or
    /// after `export`, `readonly`, `local`, `declare` or `typeset`; and the
    /// words a `for NAME in` loop goes through. An assignment before a
    /// program is that program's environment, not the line's.
    pub(super) fn assign(&mut self, words: &[String]) {
        let start = words
            .iter()
            .position(|word| !KEYWORDS.contains(&word.as_str()))
            .unwrap_or(words.len());
        let words = &words[start..];
        match words.first().map(String::as_str) {
            Some("for") => {
                if let (Some(name), Some("in")) = (words.get(1), words.get(2).map(String::as_str)) {
                    if is_name(name) {
                        let values = words[3..]
                            .iter()
                            .flat_map(|word| self.choices(word).into_iter().flatten())
                            .take(RUNS)
                            .collect();
                        self.put(name, values);
                    }
                }
            }
            Some("export" | "readonly" | "local" | "declare" | "typeset") => {
                for (name, value) in words[1..].iter().filter_map(|word| assignment(word)) {
                    self.set(name, value);
                }
            }
            Some(_) if words.iter().all(|word| assignment(word).is_some()) => {
                for (name, value) in words.iter().filter_map(|word| assignment(word)) {
                    self.set(name, value);
                }
            }
            _ => {}
        }
    }
}
