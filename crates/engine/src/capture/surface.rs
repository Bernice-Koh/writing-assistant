//! What the focused document belongs to, as [`super::Capture::surface`] answers it: the identity
//! the settings' register map is read with ([`crate::store::Config::register_for`]).

use std::fmt;

/// The surface the focused document belongs to: the application, and the page where the document
/// sits in a browser tab that surfaces one.
///
/// `Debug` shows the page URL's host alone and the document key's length, so neither a log line
/// nor a test's failure message holds a URL's path, query or fragment, or a document's name.
#[derive(Clone, PartialEq, Eq)]
pub struct Surface {
    /// The executable file name of the focused element's process, as the process table spells it,
    /// such as `WINWORD.EXE` or `msedge.exe`.
    pub process_name: String,
    /// The URL of the page that holds the focused element, where the application surfaces one.
    pub page_url: Option<String>,
    /// A key that names the document from one session to the next, for grouping what is written in
    /// it: the page URL without its fragment where there is a page URL, and otherwise the document
    /// element's name where it has one.
    pub document_key: Option<String>,
}

impl Surface {
    /// The page URL's host, as a URL parser normalises it, or `None` with no page URL, or with a
    /// URL that has no host, such as a `file:` URL of a local path.
    pub fn page_host(&self) -> Option<String> {
        self.page_url.as_deref().and_then(host_of)
    }

    /// The process name and the page URL's host alone, for a log line.
    pub fn describe(&self) -> String {
        match (&self.page_url, self.page_host()) {
            (None, _) => format!("{:?} with no page URL", self.process_name),
            (Some(_), None) => format!("{:?} with a page URL that has no host", self.process_name),
            (Some(_), Some(host)) => format!("{:?} with a page on {host:?}", self.process_name),
        }
    }
}

impl fmt::Debug for Surface {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Surface")
            .field("process_name", &self.process_name)
            .field("page_url_present", &self.page_url.is_some())
            .field("page_host", &self.page_host())
            .field(
                "document_key_length",
                &self.document_key.as_ref().map(|key| key.chars().count()),
            )
            .finish()
    }
}

/// The host of `url`, or `None` when it does not parse as a URL or has no host.
fn host_of(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    parsed
        .host_str()
        .filter(|host| !host.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on_page(url: &str) -> Surface {
        Surface {
            process_name: "msedge.exe".to_owned(),
            page_url: Some(url.to_owned()),
            document_key: Some(url.to_owned()),
        }
    }

    #[test]
    fn the_host_of_a_web_page_is_its_host_alone() {
        assert_eq!(
            on_page("https://user:secret@Mail.Example.com:8443/inbox?q=draft#reply").page_host(),
            Some("mail.example.com".to_owned())
        );
        assert_eq!(
            on_page("http://localhost:8765/page.html").page_host(),
            Some("localhost".to_owned())
        );
    }

    #[test]
    fn a_local_file_url_and_no_url_have_no_host() {
        assert_eq!(
            on_page("file:///C:/Users/notes/page.html").page_host(),
            None
        );
        let desktop = Surface {
            process_name: "WINWORD.EXE".to_owned(),
            page_url: None,
            document_key: Some("Report.docx".to_owned()),
        };
        assert_eq!(desktop.page_host(), None);
    }

    #[test]
    fn neither_debug_nor_the_description_holds_more_of_a_url_than_its_host() {
        let surface = on_page("https://example.com/private/path?query=secret#fragment");
        for shown in [format!("{surface:?}"), surface.describe()] {
            assert!(shown.contains("example.com"), "{shown}");
            for hidden in ["private", "path", "query", "secret", "fragment", "https"] {
                assert!(!shown.contains(hidden), "{hidden} in {shown}");
            }
        }
    }
}
