//! `AndroidManifest.xml`: the components the Android framework starts.
//!
//! An `<activity>`, `<service>`, `<receiver>` or `<provider>` is instantiated
//! by the platform from this file, so no call in the corpus constructs the
//! class it names. The class's only evidence of use lives here, and without
//! reading it `BootReceiver` is "live" only because it happens to be public.
//!
//! The reader is a bounded tag scanner, not an XML parser: the only facts it
//! wants are the `android:name` attribute of five tag names, and a dependency
//! for that would cost more than the scan. It is strict about what it cannot
//! read — an unterminated tag, quote or comment is an `Err`, which the caller
//! turns into a recorded parse failure rather than an empty answer, because a
//! manifest read as "no components" would silently un-exempt every one.

/// The tags whose `android:name` is a class the framework instantiates.
const COMPONENT_TAGS: &[&str] = &[
    "activity",
    "activity-alias",
    "service",
    "receiver",
    "provider",
];

/// More components than any real manifest declares; past it the scan stops and
/// reports the cut instead of letting a hostile file make the list unbounded.
const MAX_COMPONENTS: usize = 4096;

/// One component declaration: the tag that declared it and the class name as
/// written (`com.fx.app.BootReceiver`, `.MainActivity`, or a bare `Name`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AndroidComponent {
    pub tag: String,
    pub name: String,
}

/// Whether `path` names an Android manifest.
pub fn is_android_manifest(path: &str) -> bool {
    path.replace('\\', "/")
        .rsplit('/')
        .next()
        .is_some_and(|name| name == "AndroidManifest.xml")
}

/// The components `source` declares, or why it could not be read.
pub fn parse_components(source: &str) -> Result<Vec<AndroidComponent>, String> {
    let bytes = source.as_bytes();
    let mut components = Vec::new();
    let mut at = 0usize;
    while let Some(offset) = source[at..].find('<') {
        at += offset;
        let rest = &source[at..];
        if rest.starts_with("<!--") {
            at += end_of(rest, "-->", "comment")?;
            continue;
        }
        if rest.starts_with("<![CDATA[") {
            at += end_of(rest, "]]>", "CDATA section")?;
            continue;
        }
        if rest.starts_with("<?") {
            at += end_of(rest, "?>", "processing instruction")?;
            continue;
        }
        if rest.starts_with("</") || rest.starts_with("<!") {
            at += end_of(rest, ">", "tag")?;
            continue;
        }
        // An opening tag: name, attributes, `>` or `/>`.
        let name_start = at + 1;
        let mut cursor = name_start;
        while cursor < bytes.len()
            && !bytes[cursor].is_ascii_whitespace()
            && bytes[cursor] != b'>'
            && bytes[cursor] != b'/'
        {
            cursor += 1;
        }
        let tag = &source[name_start..cursor];
        if tag.is_empty() {
            return Err(format!("a `<` at byte {at} opens no tag"));
        }
        let mut name = None;
        let mut target = None;
        loop {
            while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
            match bytes.get(cursor) {
                None => return Err(format!("tag `<{tag}` is not closed")),
                Some(b'>') => {
                    cursor += 1;
                    break;
                }
                Some(b'/') => {
                    cursor += 1;
                    if bytes.get(cursor) != Some(&b'>') {
                        return Err(format!("tag `<{tag}` has a stray `/`"));
                    }
                    cursor += 1;
                    break;
                }
                Some(_) => {}
            }
            let key_start = cursor;
            while cursor < bytes.len()
                && !bytes[cursor].is_ascii_whitespace()
                && !matches!(bytes[cursor], b'=' | b'>' | b'/')
            {
                cursor += 1;
            }
            let key = &source[key_start..cursor];
            while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
            if bytes.get(cursor) != Some(&b'=') {
                return Err(format!("attribute `{key}` of `<{tag}` has no value"));
            }
            cursor += 1;
            while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
            let quote = match bytes.get(cursor) {
                Some(&quote @ (b'"' | b'\'')) => quote,
                _ => return Err(format!("attribute `{key}` of `<{tag}` is not quoted")),
            };
            cursor += 1;
            let value_start = cursor;
            while cursor < bytes.len() && bytes[cursor] != quote {
                cursor += 1;
            }
            if cursor >= bytes.len() {
                return Err(format!("attribute `{key}` of `<{tag}` is not terminated"));
            }
            let value = &source[value_start..cursor];
            cursor += 1;
            match key {
                "android:name" => name = Some(value),
                "android:targetActivity" => target = Some(value),
                _ => {}
            }
        }
        at = cursor;
        if !COMPONENT_TAGS.contains(&tag) {
            continue;
        }
        // An alias declares no class of its own; the activity it targets is
        // the one the framework starts.
        let declared = if tag == "activity-alias" {
            target
        } else {
            name
        };
        if let Some(declared) = declared.filter(|declared| is_class_name(declared)) {
            if components.len() >= MAX_COMPONENTS {
                return Err(format!(
                    "more than {MAX_COMPONENTS} components; the rest were not read"
                ));
            }
            components.push(AndroidComponent {
                tag: tag.to_string(),
                name: declared.to_string(),
            });
        }
    }
    Ok(components)
}

/// Byte offset just past the first `close` in `rest`, or an error naming what
/// was left open.
fn end_of(rest: &str, close: &str, what: &str) -> Result<usize, String> {
    rest.find(close)
        .map(|found| found + close.len())
        .ok_or_else(|| format!("unterminated {what}"))
}

/// A class name a manifest can write: dotted identifiers, optionally led by a
/// `.` for the module's namespace. A build placeholder (`${applicationId}`) is
/// not a class and is refused.
fn is_class_name(name: &str) -> bool {
    let body = name.strip_prefix('.').unwrap_or(name);
    !body.is_empty()
        && body.split('.').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '$'))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = r#"<?xml version="1.0"?>
<manifest xmlns:android="http://schemas.android.com/apk/res/android">
    <!-- <receiver android:name=".Commented"/> -->
    <application android:name=".FxApp">
        <activity android:name=".MainActivity" android:exported="true">
            <intent-filter><action android:name="android.intent.action.MAIN"/></intent-filter>
        </activity>
        <receiver android:name='com.fx.app.BootReceiver'/>
        <activity-alias android:name=".Alias" android:targetActivity=".Target"/>
        <provider android:name="${applicationId}.Provider"/>
    </application>
</manifest>"#;

    #[test]
    fn components_are_the_named_classes_the_framework_starts() {
        let found = parse_components(MANIFEST).expect("reads");
        let names: Vec<&str> = found.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            [".MainActivity", "com.fx.app.BootReceiver", ".Target"],
            "a comment, an intent-filter action, an alias's own name and a build placeholder are not components: {found:?}"
        );
    }

    #[test]
    fn a_manifest_that_cannot_be_read_is_an_error_not_an_empty_answer() {
        for broken in [
            "<manifest><receiver android:name=\"a.B\"",
            "<manifest><receiver android:name=\"a.B/></manifest>",
            "<manifest><!-- never closed <receiver android:name=\"a.B\"/>",
            "<manifest><receiver android:name=a.B/></manifest>",
        ] {
            assert!(
                parse_components(broken).is_err(),
                "{broken:?} must be refused, not read as having no components"
            );
        }
    }

    #[test]
    fn the_component_list_is_bounded() {
        let mut source = String::from("<manifest>");
        for index in 0..=MAX_COMPONENTS {
            source.push_str(&format!("<receiver android:name=\"a.R{index}\"/>"));
        }
        source.push_str("</manifest>");
        let error = parse_components(&source).expect_err("over the cap");
        assert!(error.contains("more than"), "{error}");
    }
}
