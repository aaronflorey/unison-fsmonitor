use percent_encoding::{NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use std::borrow::Cow;

pub fn encode(value: &str) -> String {
    utf8_percent_encode(value, NON_ALPHANUMERIC).to_string()
}

fn decode(value: &str) -> Cow<'_, str> {
    percent_decode_str(value).decode_utf8_lossy()
}

pub fn parse_input(input: &str) -> (String, Vec<String>) {
    let mut parts = input.split_whitespace();
    let command = parts.next().unwrap_or_default().to_owned();
    let args = parts.map(|word| decode(word).into_owned()).collect();

    (command, args)
}

#[cfg(test)]
mod tests {
    use super::{encode, parse_input};

    #[test]
    fn encode_escapes_reserved_characters() {
        assert_eq!(encode("before%after"), "before%25after");
    }

    #[test]
    fn parse_input_decodes_arguments() {
        assert_eq!(
            parse_input("START 123 foo%20bar baz%2Fqux\n"),
            (
                "START".to_owned(),
                vec!["123".to_owned(), "foo bar".to_owned(), "baz/qux".to_owned()],
            )
        );
    }
}
