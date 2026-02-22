use quick_xml::events::Event;
use quick_xml::Reader;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum OpmlImportError {
    #[error("XML parsing error: {0}")]
    XmlError(#[from] quick_xml::Error),
}

#[derive(Debug, Clone)]
struct Outline {
    text: Option<String>,
    title: Option<String>,
    xml_url: Option<String>,
    html_url: Option<String>,
    children: Vec<Outline>,
    other_attrs: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct OpmlFeed {
    pub url: String,
    pub title: Option<String>,
    pub website_url: Option<String>,
    pub opml_attributes: Option<String>,
}

#[derive(Debug)]
pub struct OpmlImportResult {
    pub feeds: Vec<OpmlFeed>,
    pub errors: Vec<String>,
}

pub fn parse_opml(opml_content: &str) -> Result<OpmlImportResult, OpmlImportError> {
    println!("[DEBUG] Parsing OPML with manual parser...");

    let mut reader = Reader::from_str(opml_content);
    reader.config_mut().trim_text(false);

    let mut results: Vec<Outline> = Vec::new();
    let mut stack: Vec<Outline> = Vec::new();
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                if e.name().as_ref() == b"outline" {
                    let mut outline = Outline {
                        text: None,
                        title: None,
                        xml_url: None,
                        html_url: None,
                        children: Vec::new(),
                        other_attrs: Vec::new(),
                    };

                    for attr in e.attributes().with_checks(false).flatten() {
                        let key = std::str::from_utf8(attr.key.as_ref())
                            .unwrap_or("")
                            .to_string();
                        let value = std::str::from_utf8(&attr.value).unwrap_or("").to_string();

                        match key.as_str() {
                            "text" => outline.text = Some(value),
                            "title" => outline.title = Some(value),
                            "xmlUrl" => outline.xml_url = Some(value),
                            "htmlUrl" => outline.html_url = Some(value),
                            _ => outline.other_attrs.push((key, value)),
                        }
                    }

                    println!(
                        "[DEBUG] Outline: text={:?}, xmlUrl={:?}",
                        outline.text, outline.xml_url
                    );

                    stack.push(outline);
                }
            }
            Ok(Event::End(e)) => {
                if e.name().as_ref() == b"outline" {
                    if let Some(child) = stack.pop() {
                        if stack.is_empty() {
                            results.push(child);
                        } else {
                            stack.last_mut().unwrap().children.push(child);
                        }
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(OpmlImportError::XmlError(e)),
            _ => {}
        }
        buf.clear();
    }

    println!("[DEBUG] Parsed {} top-level outlines", results.len());

    // Extract all feeds from the outline tree
    let mut feeds = Vec::new();
    for outline in &results {
        extract_feeds(outline, &mut feeds);
    }

    println!("[DEBUG] Extracted {} feeds from OPML", feeds.len());

    Ok(OpmlImportResult {
        feeds,
        errors: Vec::new(),
    })
}

fn extract_feeds(outline: &Outline, feeds: &mut Vec<OpmlFeed>) {
    if let Some(xml_url) = &outline.xml_url {
        let feed = OpmlFeed {
            url: xml_url.clone(),
            title: outline.title.clone().or(outline.text.clone()),
            website_url: outline.html_url.clone(),
            opml_attributes: if outline.other_attrs.is_empty() {
                None
            } else {
                serde_json::to_string(&outline.other_attrs).ok()
            },
        };
        println!("[DEBUG] Feed: url={}, title={:?}", feed.url, feed.title);
        feeds.push(feed);
    }

    for child in &outline.children {
        extract_feeds(child, feeds);
    }
}
