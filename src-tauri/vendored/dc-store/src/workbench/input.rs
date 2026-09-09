use rusqlite::Connection;

use super::{Error, Result};

/// SQLite's bundled JSON parser supplies validation and encoding. Inputs never
/// become SQL: field paths are program constants and values are bound parameters.
pub(super) struct Input<'a> {
    pub conn: &'a Connection,
    pub raw: &'a str,
}

impl<'a> Input<'a> {
    pub fn new(conn: &'a Connection, raw: &'a str) -> Result<Self> {
        if raw.len() > 256 * 1024 {
            return Err(Error::invalid("request exceeds 256 KiB"));
        }
        let valid: bool = conn.query_row("SELECT json_valid(?1)", [raw], |r| r.get(0))?;
        if !valid {
            return Err(Error::invalid("request is not valid JSON"));
        }
        let object: bool = conn.query_row("SELECT json_type(?1)='object'", [raw], |r| r.get(0))?;
        if !object {
            return Err(Error::invalid("request must be a JSON object"));
        }
        // json_valid accepts lone UTF-16 surrogate escapes. Extracting those
        // into a Rust String fails as a SQL conversion error, which used to
        // misreport malformed user text as unavailable storage. Validate every
        // decoded key/value at the request boundary, including array entries.
        let mut text_values = conn.prepare("SELECT key,atom FROM json_tree(?1)")?;
        let mut rows = text_values.query([raw])?;
        while let Some(row) = rows.next()? {
            for column in 0..2 {
                if let rusqlite::types::ValueRef::Text(bytes) = row.get_ref(column)? {
                    let value = std::str::from_utf8(bytes)
                        .map_err(|_| Error::invalid("request contains invalid Unicode"))?;
                    if value.contains('\0') {
                        return Err(Error::invalid("request contains NUL"));
                    }
                }
            }
        }
        let duplicate: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM json_tree(?1) WHERE parent IS NOT NULL GROUP BY parent,key HAVING count(*)>1)",
            [raw], |r| r.get(0))?;
        if duplicate {
            return Err(Error::invalid("duplicate JSON keys are not allowed"));
        }
        Ok(Self { conn, raw })
    }

    pub fn fields(&self, allowed: &[&str]) -> Result<()> {
        let mut stmt = self.conn.prepare("SELECT key FROM json_each(?1)")?;
        for key in stmt.query_map([self.raw], |r| r.get::<_, String>(0))? {
            let key = key?;
            if !allowed.contains(&key.as_str()) {
                return Err(Error::invalid(format!("unknown field {key}")));
            }
        }
        Ok(())
    }

    pub fn kind(&self, key: &str) -> Result<Option<String>> {
        Ok(self.conn.query_row(
            "SELECT json_type(?1,?2)",
            [self.raw, &format!("$.{key}")],
            |r| r.get(0),
        )?)
    }

    pub fn text(&self, key: &str, max: usize) -> Result<Option<String>> {
        match self.kind(key)?.as_deref() {
            None | Some("null") => Ok(None),
            Some("text") => {
                let value: String = self.conn.query_row(
                    "SELECT json_extract(?1,?2)",
                    [self.raw, &format!("$.{key}")],
                    |r| r.get(0),
                )?;
                if value.len() > max || value.contains('\0') {
                    return Err(Error::invalid(format!(
                        "{key} exceeds its bound or contains NUL"
                    )));
                }
                Ok(Some(value))
            }
            _ => Err(Error::invalid(format!("{key} must be a string"))),
        }
    }

    pub fn required_text(&self, key: &str, max: usize) -> Result<String> {
        let value = self
            .text(key, max)?
            .ok_or_else(|| Error::invalid(format!("{key} is required")))?;
        if value.trim().is_empty() {
            return Err(Error::invalid(format!("{key} must not be blank")));
        }
        Ok(value)
    }

    pub fn id(&self, key: &str) -> Result<String> {
        let value = self.required_text(key, 128)?;
        if !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        {
            return Err(Error::invalid(format!(
                "{key} must be an opaque alphanumeric identifier"
            )));
        }
        Ok(value)
    }

    pub fn optional_id(&self, key: &str) -> Result<Option<String>> {
        if self.text(key, 128)?.is_some() {
            self.id(key).map(Some)
        } else {
            Ok(None)
        }
    }

    pub fn integer(&self, key: &str, default: Option<i64>, max: i64) -> Result<i64> {
        match self.kind(key)?.as_deref() {
            None | Some("null") => {
                default.ok_or_else(|| Error::invalid(format!("{key} is required")))
            }
            Some("integer") => {
                let exact: bool = self.conn.query_row(
                    "SELECT typeof(json_extract(?1,?2))='integer'",
                    [self.raw, &format!("$.{key}")],
                    |r| r.get(0),
                )?;
                if !exact {
                    return Err(Error::invalid(format!("{key} is out of range")));
                }
                let value: i64 = self.conn.query_row(
                    "SELECT json_extract(?1,?2)",
                    [self.raw, &format!("$.{key}")],
                    |r| r.get(0),
                )?;
                if !(0..=max).contains(&value) {
                    return Err(Error::invalid(format!("{key} is out of range")));
                }
                Ok(value)
            }
            _ => Err(Error::invalid(format!("{key} must be an integer"))),
        }
    }

    pub fn boolean(&self, key: &str, default: bool) -> Result<bool> {
        match self.kind(key)?.as_deref() {
            None | Some("null") => Ok(default),
            Some("true") => Ok(true),
            Some("false") => Ok(false),
            _ => Err(Error::invalid(format!("{key} must be a boolean"))),
        }
    }

    pub fn strings(
        &self,
        key: &str,
        max_count: usize,
        max_bytes: usize,
        required: bool,
    ) -> Result<Vec<String>> {
        match self.kind(key)?.as_deref() {
            None | Some("null") if !required => return Ok(Vec::new()),
            Some("array") => {}
            _ => return Err(Error::invalid(format!("{key} must be an array"))),
        }
        let mut statement = self
            .conn
            .prepare("SELECT type,value FROM json_each(?1,?2)")?;
        let mut rows = statement.query([self.raw, &format!("$.{key}")])?;
        let mut values = Vec::new();
        while let Some(row) = rows.next()? {
            if row.get::<_, String>(0)? != "text" {
                return Err(Error::invalid(format!("{key} must contain strings")));
            }
            let value: String = row.get(1)?;
            if values.len() >= max_count
                || value.len() > max_bytes
                || value.contains('\0')
                || value.trim().is_empty()
            {
                return Err(Error::invalid(format!(
                    "{key} contains an invalid or oversized entry"
                )));
            }
            values.push(value);
        }
        Ok(values)
    }

    pub fn normalized(&self) -> Result<String> {
        Ok(self
            .conn
            .query_row("SELECT json(?1)", [self.raw], |r| r.get(0))?)
    }
}
