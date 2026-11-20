use serde_json::{Map, Value, json};

pub(crate) const FRAME_LIMIT: usize = 256 * 1024;
pub(crate) type Params = Map<String, Value>;
pub(crate) type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Copy)]
pub(crate) enum Error {
    Parse,
    Envelope,
    Unsupported,
    Invalid,
    Internal,
    Denied,
    TooLarge,
    RetryAfter(u64),
}

impl Error {
    pub fn frame(self, id: Option<&str>) -> Value {
        let (code, message) = match self {
            Self::Parse => (-32700, "Invalid JSON"),
            Self::Envelope => (-32600, "Invalid request envelope"),
            Self::Unsupported => (-32601, "Unsupported method or feature"),
            Self::Invalid => (-32602, "Invalid parameters"),
            Self::Internal => (-32603, "Internal server error"),
            Self::Denied => (-32001, "The server does not allow this request"),
            Self::RetryAfter(_) => (-32002, "Too many requests"),
            Self::TooLarge => (-32003, "Value exceeds the server limit"),
        };
        let mut frame = json!({"error": {"code": code, "message": message}});
        if let Self::RetryAfter(seconds) = self {
            frame["error"]["data"] = json!({"retry_after": seconds});
        }
        if let Some(id) = id {
            frame["id"] = json!(id);
        }
        frame
    }
}

pub(crate) struct Request {
    pub id: Option<String>,
    pub method: String,
    pub params: Value,
}

impl Request {
    pub fn parse(text: &str) -> Result<Self> {
        let frame: Value = serde_json::from_str(text).map_err(|_| Error::Parse)?;
        let object = frame.as_object().ok_or(Error::Envelope)?;
        let method = object
            .get("method")
            .and_then(Value::as_str)
            .ok_or(Error::Envelope)?;
        let id = string(object, "id").map_err(|_| Error::Envelope)?;
        Ok(Self {
            id: id.map(str::to_owned),
            method: method.to_owned(),
            params: object.get("params").cloned().unwrap_or_else(|| json!({})),
        })
    }
}

pub(crate) fn string<'a>(params: &'a Params, key: &str) -> Result<Option<&'a str>> {
    params
        .get(key)
        .map(|value| value.as_str().ok_or(Error::Invalid))
        .transpose()
}

pub(crate) fn object<'a>(params: &'a Params, key: &str) -> Result<Option<&'a Params>> {
    params
        .get(key)
        .map(|value| value.as_object().ok_or(Error::Invalid))
        .transpose()
}

pub(crate) fn boolean(params: &Params, key: &str) -> Result<bool> {
    params
        .get(key)
        .map_or(Ok(false), |value| value.as_bool().ok_or(Error::Invalid))
}

pub(crate) fn notification(method: &str, params: Value) -> Value {
    json!({"method": method, "params": params})
}

#[cfg(test)]
mod tests {
    //! §1.1: error codes and envelope parsing.
    use super::*;

    fn code(error: Error) -> i64 {
        error.frame(None)["error"]["code"].as_i64().unwrap()
    }

    fn parse_error(text: &str) -> i64 {
        match Request::parse(text) {
            Ok(request) => panic!("{text} parsed as {}", request.method),
            Err(error) => code(error),
        }
    }

    #[test]
    fn error_codes_match_the_spec_table() {
        let table = [
            (Error::Parse, -32700),
            (Error::Envelope, -32600),
            (Error::Unsupported, -32601),
            (Error::Invalid, -32602),
            (Error::Internal, -32603),
            (Error::Denied, -32001),
            (Error::RetryAfter(30), -32002),
            (Error::TooLarge, -32003),
        ];
        for (error, expected) in table {
            let frame = error.frame(None);
            assert_eq!(code(error), expected);
            assert!(
                frame["error"]["message"]
                    .as_str()
                    .is_some_and(|m| !m.is_empty())
            );
            assert!(
                frame.get("id").is_none(),
                "an error that is not a reply has no id"
            );
        }
    }

    #[test]
    fn error_frames_carry_the_request_id_and_retry_delay() {
        let frame = Error::RetryAfter(30).frame(Some("c43"));
        assert_eq!(frame["id"], "c43");
        assert_eq!(frame["error"]["data"], json!({"retry_after": 30}));
        assert!(
            Error::Denied.frame(Some("c1"))["error"]
                .get("data")
                .is_none()
        );
    }

    #[test]
    fn invalid_json_is_a_parse_error() {
        for text in [
            "",
            "{",
            "{\"method\":\"ping\",}",
            "nul",
            "{'method':'ping'}",
        ] {
            assert_eq!(parse_error(text), -32700, "{text:?}");
        }
    }

    #[test]
    fn a_non_object_or_a_bad_method_or_id_is_an_invalid_envelope() {
        for text in [
            "[]",
            "\"ping\"",
            "null",
            "42",
            "{}",
            r#"{"method":1}"#,
            r#"{"method":null}"#,
            r#"{"method":["ping"]}"#,
            r#"{"params":{}}"#,
            r#"{"method":"me","id":7}"#,
            r#"{"method":"me","id":null}"#,
        ] {
            assert_eq!(parse_error(text), -32600, "{text}");
        }
    }

    #[test]
    fn a_valid_envelope_keeps_method_id_and_params() {
        let request =
            Request::parse(r#"{"method":"message","id":"c3","params":{"x":1},"extra":true}"#)
                .unwrap_or_else(|error| panic!("{}", error.frame(None)));
        assert_eq!(request.method, "message");
        assert_eq!(request.id.as_deref(), Some("c3"));
        assert_eq!(request.params, json!({"x": 1}));

        let notification = Request::parse(r#"{"method":"ping"}"#)
            .unwrap_or_else(|error| panic!("{}", error.frame(None)));
        assert_eq!(notification.id, None);
        assert_eq!(notification.params, json!({}), "absent params read as {{}}");
    }
}
