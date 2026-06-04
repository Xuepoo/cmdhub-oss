use flate2::read::GzDecoder;
use std::collections::HashMap;
use std::io::Read;

pub struct Tokenizer {
    vocab: HashMap<String, u32>,
}

impl Default for Tokenizer {
    fn default() -> Self {
        Self::new()
    }
}

impl Tokenizer {
    pub fn new() -> Self {
        let compressed = include_bytes!("tokenizer/assets/vocab.txt.gz");
        let mut decoder = GzDecoder::new(&compressed[..]);
        let mut s = String::new();
        decoder
            .read_to_string(&mut s)
            .expect("Failed to decompress vocabulary asset");
        let mut vocab = HashMap::new();
        for (idx, line) in s.lines().enumerate() {
            vocab.insert(line.to_string(), idx as u32);
        }
        Self { vocab }
    }

    /// Preprocesses text by lowercasing and splitting punctuation to match BERT tokenization rules.
    fn preprocess_text(&self, text: &str) -> String {
        let mut preprocessed = String::new();
        for c in text.chars() {
            if c.is_ascii_punctuation() {
                preprocessed.push(' ');
                preprocessed.push(c);
                preprocessed.push(' ');
            } else {
                preprocessed.push(c);
            }
        }
        preprocessed.to_lowercase()
    }

    /// Performs WordPiece tokenization on a single word.
    fn tokenize_word(&self, word: &str) -> Vec<i64> {
        if word.is_empty() {
            return vec![];
        }
        if let Some(&id) = self.vocab.get(word) {
            return vec![id as i64];
        }

        let char_indices: Vec<(usize, char)> = word.char_indices().collect();
        let mut start = 0;
        let mut sub_tokens = Vec::new();

        while start < char_indices.len() {
            let mut end = char_indices.len();
            let mut cur_sub_token_id = None;
            let mut cur_end = start;

            while start < end {
                let substr = &word[char_indices[start].0..if end < char_indices.len() {
                    char_indices[end].0
                } else {
                    word.len()
                }];
                let lookup_str = if start > 0 {
                    format!("##{}", substr)
                } else {
                    substr.to_string()
                };

                if let Some(&id) = self.vocab.get(&lookup_str) {
                    cur_sub_token_id = Some(id as i64);
                    cur_end = end;
                    break;
                }
                end -= 1;
            }

            if let Some(id) = cur_sub_token_id {
                sub_tokens.push(id);
                start = cur_end;
            } else {
                // If any sub-word cannot be resolved, return [UNK] (ID 100) for the entire word
                return vec![100];
            }
        }
        sub_tokens
    }

    pub fn tokenize_query(&self, text: &str) -> (Vec<i64>, Vec<i64>) {
        let prefix = "Represent this sentence for searching relevant passages: ";
        let query = format!("{}{}", prefix, text);

        let preprocessed = self.preprocess_text(&query);
        let mut token_ids = vec![101]; // [CLS]

        for word in preprocessed.split_whitespace() {
            token_ids.extend(self.tokenize_word(word));
        }

        token_ids.push(102); // [SEP]

        let len = token_ids.len();
        let mut attention_mask = vec![1; len];

        if token_ids.len() > 512 {
            token_ids.truncate(512);
            attention_mask.truncate(512);
        } else {
            while token_ids.len() < 512 {
                token_ids.push(0); // [PAD]
                attention_mask.push(0);
            }
        }

        (token_ids, attention_mask)
    }

    pub fn tokenize_passage(&self, text: &str) -> (Vec<i64>, Vec<i64>) {
        let preprocessed = self.preprocess_text(text);
        let mut token_ids = vec![101]; // [CLS]

        for word in preprocessed.split_whitespace() {
            token_ids.extend(self.tokenize_word(word));
        }

        token_ids.push(102); // [SEP]

        let len = token_ids.len();
        let mut attention_mask = vec![1; len];

        if token_ids.len() > 512 {
            token_ids.truncate(512);
            attention_mask.truncate(512);
        } else {
            while token_ids.len() < 512 {
                token_ids.push(0); // [PAD]
                attention_mask.push(0);
            }
        }

        (token_ids, attention_mask)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tokenizer_prefix_and_padding() {
        let tokenizer = Tokenizer::new();
        let (ids, mask) = tokenizer.tokenize_query("test query");

        assert_eq!(ids.len(), 512);
        assert_eq!(mask.len(), 512);

        // CLS position
        assert_eq!(ids[0], 101);

        // Attention mask matches valid tokens
        let mut valid_count = 0;
        for &m in &mask {
            if m == 1 {
                valid_count += 1;
            }
        }

        assert!(valid_count > 2); // CLS, SEP, plus query and prefix tokens
        assert_eq!(ids[valid_count - 1], 102); // SEP position

        // Rest of the array is padded with 0
        for i in valid_count..512 {
            assert_eq!(ids[i], 0);
            assert_eq!(mask[i], 0);
        }

        // Verify the prefix is tokenized as part of the query.
        // The first few tokens after CLS should correspond to "represent", "this", "sentence"
        // Let's verify that IDs match.
        assert_eq!(ids[1], *tokenizer.vocab.get("represent").unwrap() as i64);
        assert_eq!(ids[2], *tokenizer.vocab.get("this").unwrap() as i64);
        assert_eq!(ids[3], *tokenizer.vocab.get("sentence").unwrap() as i64);
    }
}
