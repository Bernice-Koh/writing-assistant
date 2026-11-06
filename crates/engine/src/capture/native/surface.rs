//! The surface of an element: its page URL and its document key, beside the process name the
//! process table gives.
//!
//! Chromium exposes a page as an element of the Document control type, and each inner frame as
//! another inside it; the outermost of Chromium's, the page's root document, carries the page's
//! URL. The UI Automation bridge can surface it through the root document's `ValuePattern` value
//! or through its `LegacyIAccessiblePattern` value, which Chromium sets to the document's URL for
//! MSAA's document role. Each is read in that order and taken only when it is a page URL by
//! [`page_url_from`]. Only a document of Chromium's own framework is asked, since another
//! application's document element can hold its document's text as its value.

use windows::Win32::UI::Accessibility::{
    IUIAutomation, IUIAutomationElement, IUIAutomationLegacyIAccessiblePattern,
    IUIAutomationValuePattern, UIA_ClassNamePropertyId, UIA_ControlTypePropertyId,
    UIA_DocumentControlTypeId, UIA_FrameworkIdPropertyId, UIA_LegacyIAccessiblePatternId,
    UIA_NamePropertyId, UIA_ProcessIdPropertyId, UIA_ValuePatternId,
};

use super::error::NativeCaptureError;
use crate::capture::Surface;

/// The framework id Chromium reports for the elements of a page's accessibility tree.
pub const CHROMIUM_FRAMEWORK_ID: &str = "Chrome";

/// The schemes a page URL is taken with: a web page's and a local file's.
const PAGE_URL_SCHEMES: [&str; 3] = ["http", "https", "file"];

/// Caps the walk up an element's ancestors, against a tree whose parent links cycle.
const MAX_ANCESTORS: usize = 64;

/// The property of a document element that carried its page URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlProperty {
    /// The value of its `ValuePattern`.
    Value,
    /// The value of its `LegacyIAccessiblePattern`, MSAA's `accValue`.
    LegacyIAccessibleValue,
}

impl UrlProperty {
    /// Both, in the order a root document is asked.
    pub const ALL: [Self; 2] = [Self::Value, Self::LegacyIAccessibleValue];
}

/// One read of a surface, with the property its page URL came through.
#[derive(Debug)]
pub struct SurfaceRead {
    pub surface: Surface,
    pub url_property: Option<UrlProperty>,
}

/// An element of the Document control type among an element's ancestors, the element included,
/// with the properties the walk read in the same call.
pub struct DocumentElement {
    pub element: IUIAutomationElement,
    pub framework_id: String,
    pub class_name: String,
    pub name: String,
}

/// The surface of `element`, an element of the process named `process_name`. The page URL is the
/// one the outermost Chromium document among the element's ancestors carries. The document key is
/// that URL without its fragment, and with no URL the name of the nearest document, the element
/// included, where the name holds more than whitespace. A walk of the ancestors that fails leaves
/// both `None`, logged at `debug`, so the register map is still read by the process name.
pub fn read(
    client: &IUIAutomation,
    element: &IUIAutomationElement,
    process_name: String,
) -> SurfaceRead {
    let documents = document_elements(client, element).unwrap_or_else(|error| {
        log::debug!(
            "could not walk the focused element's ancestors, so its surface has no page URL or \
             document key: {error}"
        );
        Vec::new()
    });
    let page = documents
        .iter()
        .rev()
        .find(|document| document.framework_id == CHROMIUM_FRAMEWORK_ID)
        .and_then(|root| {
            UrlProperty::ALL.into_iter().find_map(|property| {
                page_url_through(&root.element, property)
                    .ok()
                    .flatten()
                    .map(|url| (url, property))
            })
        });
    let nearest_name = documents.first().map(|nearest| nearest.name.as_str());
    let page_url = page.as_ref().map(|(url, _)| url.clone());
    SurfaceRead {
        surface: Surface {
            process_name,
            document_key: document_key(page_url.as_deref(), nearest_name),
            page_url,
        },
        url_property: page.map(|(_, property)| property),
    }
}

/// Every element of the Document control type from `element` up through its ancestors, nearest
/// first. The walk stops at the desktop's root, at the first ancestor of another process, or after
/// 64 elements, against a tree whose parent links cycle.
///
/// # Errors
/// The UI Automation call's error when a property read or the cache request fails.
pub fn document_elements(
    client: &IUIAutomation,
    element: &IUIAutomationElement,
) -> Result<Vec<DocumentElement>, NativeCaptureError> {
    // SAFETY: `client` and `element` are live and owned by the calling thread. Each call fails
    // safely (Err) on an element that has gone away, and every BSTR returned is dropped by
    // windows-rs. The cache request fetches each ancestor's properties in the call that fetches the
    // ancestor, so the walk costs one cross-process call a level.
    unsafe {
        let cache = client.CreateCacheRequest()?;
        for property in [
            UIA_ControlTypePropertyId,
            UIA_ProcessIdPropertyId,
            UIA_FrameworkIdPropertyId,
            UIA_ClassNamePropertyId,
            UIA_NamePropertyId,
        ] {
            cache.AddProperty(property)?;
        }
        let walker = client.RawViewWalker()?;
        let mut current = element.BuildUpdatedCache(&cache)?;
        let process_id = current.CachedProcessId()?;
        let mut documents = Vec::new();
        for _ in 0..MAX_ANCESTORS {
            if current.CachedControlType()? == UIA_DocumentControlTypeId {
                documents.push(DocumentElement {
                    framework_id: current.CachedFrameworkId()?.to_string(),
                    class_name: current.CachedClassName()?.to_string(),
                    name: current.CachedName()?.to_string(),
                    element: current.clone(),
                });
            }
            // The desktop's root has no parent.
            let Ok(parent) = walker.GetParentElementBuildCache(&current, &cache) else {
                break;
            };
            if parent.CachedProcessId()? != process_id {
                break;
            }
            current = parent;
        }
        Ok(documents)
    }
}

/// The page URL that `element`'s `property` carries, by [`page_url_from`], or `None` when its value
/// is not one.
///
/// # Errors
/// The UI Automation call's error when the element does not support the property's pattern or the
/// read fails.
pub fn page_url_through(
    element: &IUIAutomationElement,
    property: UrlProperty,
) -> Result<Option<String>, NativeCaptureError> {
    // SAFETY: `element` is live; GetCurrentPatternAs fails safely when the pattern is unsupported,
    // and the pattern is read only once obtained.
    let value = unsafe {
        match property {
            UrlProperty::Value => element
                .GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)?
                .CurrentValue()?,
            UrlProperty::LegacyIAccessibleValue => element
                .GetCurrentPatternAs::<IUIAutomationLegacyIAccessiblePattern>(
                    UIA_LegacyIAccessiblePatternId,
                )?
                .CurrentValue()?,
        }
    };
    Ok(page_url_from(&value.to_string()))
}

/// `value` as a page URL: the URL it parses as, normalised, when it holds no whitespace and its
/// scheme is a web page's or a local file's, and `None` for any other value. A URL holds no
/// whitespace, while a document's text that begins with a URL does.
pub fn page_url_from(value: &str) -> Option<String> {
    if value.chars().any(char::is_whitespace) {
        return None;
    }
    let url = url::Url::parse(value).ok()?;
    PAGE_URL_SCHEMES.contains(&url.scheme()).then(|| url.into())
}

/// The document key of a surface with `page_url` whose nearest document element is named
/// `document_name`: the page URL without its fragment where there is one, since a fragment names a
/// place within one page, and otherwise the name where it holds more than whitespace.
fn document_key(page_url: Option<&str>, document_name: Option<&str>) -> Option<String> {
    match page_url {
        Some(url) => Some(without_fragment(url).to_owned()),
        None => document_name
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned),
    }
}

/// `url` up to its fragment's `#`, or the whole of it with no fragment.
fn without_fragment(url: &str) -> &str {
    url.split_once('#').map_or(url, |(before, _)| before)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_web_page_or_a_local_file_url_is_a_page_url() {
        assert_eq!(
            page_url_from("https://example.com/notes?draft=1#end").as_deref(),
            Some("https://example.com/notes?draft=1#end")
        );
        assert_eq!(
            page_url_from("http://LOCALHOST:8765/page.html").as_deref(),
            Some("http://localhost:8765/page.html")
        );
        assert_eq!(
            page_url_from("file:///C:/pages/check.html").as_deref(),
            Some("file:///C:/pages/check.html")
        );
    }

    #[test]
    fn any_other_value_is_not_a_page_url() {
        for value in [
            "",
            "Dear team: the draft follows.",
            "Note:draft",
            "https://example.com/ and then the rest of a paragraph",
            "about:blank",
            "edge://newtab/",
            "javascript:void(0)",
        ] {
            assert_eq!(page_url_from(value), None, "{value:?}");
        }
    }

    #[test]
    fn a_page_urls_document_key_is_the_url_without_its_fragment() {
        assert_eq!(
            document_key(
                Some("https://example.com/notes?draft=1#section-2"),
                Some("Notes")
            )
            .as_deref(),
            Some("https://example.com/notes?draft=1")
        );
        assert_eq!(
            document_key(Some("file:///C:/pages/check.html"), None).as_deref(),
            Some("file:///C:/pages/check.html")
        );
    }

    #[test]
    fn with_no_page_url_the_document_key_is_the_documents_name() {
        assert_eq!(
            document_key(None, Some("Report.docx")).as_deref(),
            Some("Report.docx")
        );
        assert_eq!(document_key(None, Some("  \t")), None);
        assert_eq!(document_key(None, None), None);
    }

    #[test]
    fn stripping_the_fragment_keeps_everything_before_its_mark() {
        assert_eq!(
            without_fragment("https://a.example/p#x#y"),
            "https://a.example/p"
        );
        assert_eq!(
            without_fragment("https://a.example/p#"),
            "https://a.example/p"
        );
        assert_eq!(
            without_fragment("https://a.example/p?q=1"),
            "https://a.example/p?q=1"
        );
    }
}
